use crate::{AudioControl, AudioError, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

pub(crate) fn open(control: &AudioControl, frames: Arc<AtomicU64>) -> Result<cpal::Stream> {
    let rate = control.lock()?.config.sample_rate;
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| AudioError::new("audio.device.unavailable", "No default output device"))?;
    let configs = device.supported_output_configs().map_err(unavailable)?;
    // Preserve the exact requested stereo rate. Prefer floating point output;
    // convert through CPAL's Sample trait for integer-only devices.
    let supported = configs
        .filter(|c| c.channels() == 2 && c.min_sample_rate() <= rate && rate <= c.max_sample_rate())
        .filter(|c| {
            matches!(
                c.sample_format(),
                cpal::SampleFormat::F32 | cpal::SampleFormat::I16 | cpal::SampleFormat::U16
            )
        })
        .min_by_key(|c| match c.sample_format() {
            cpal::SampleFormat::F32 => 0,
            cpal::SampleFormat::I16 => 1,
            _ => 2,
        })
        .ok_or_else(|| {
            AudioError::new(
                "audio.device.unavailable",
                "Device has no supported stereo configuration at requested rate",
            )
        })?
        .with_sample_rate(rate);
    let format = supported.sample_format();
    let config = supported.config();
    let stream = match format {
        cpal::SampleFormat::F32 => build::<f32>(&device, config, control, frames),
        cpal::SampleFormat::I16 => build::<i16>(&device, config, control, frames),
        cpal::SampleFormat::U16 => build::<u16>(&device, config, control, frames),
        _ => unreachable!("Configuration formats are filtered above"),
    }?;
    stream.play().map_err(unavailable)?;
    Ok(stream)
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    control: &AudioControl,
    frames: Arc<AtomicU64>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let callback_control = control.clone();
    let error_control = control.clone();
    // Bounded scratch avoids allocation in the device callback even when the
    // native buffer size changes; the same mixer is called in stereo blocks.
    let mut scratch = [0.0_f32; 2048];
    device
        .build_output_stream(
            config,
            move |output: &mut [T], _| {
                output.fill(T::EQUILIBRIUM);
                let Ok(mut mixer) = callback_control.lock() else {
                    return;
                };
                if mixer.ready().is_err() {
                    return;
                }
                if !output.len().is_multiple_of(2) {
                    mixer.fail("Device returned a partial stereo frame");
                    return;
                }
                for block in output.chunks_mut(scratch.len()) {
                    let samples = &mut scratch[..block.len()];
                    mixer.mix(samples);
                    for (out, sample) in block.iter_mut().zip(samples) {
                        *out = T::from_sample(*sample);
                    }
                }
                frames.fetch_add((output.len() / 2) as u64, Ordering::Relaxed);
            },
            move |error| {
                if let Ok(mut mixer) = error_control.lock() {
                    mixer.fail(error.to_string());
                }
            },
            Some(Duration::from_secs(3)),
        )
        .map_err(unavailable)
}
fn unavailable(error: impl std::fmt::Display) -> AudioError {
    AudioError::new("audio.device.unavailable", error.to_string())
}
