//! Incremental local-file demux/decode. No I/O or codec work runs in the mixer.
use crate::{AudioError, Result, StreamGeneration, StreamWriter, valid_rate};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use symphonia::core::{
    codecs::audio::{AudioDecoder as SymDecoder, AudioDecoderOptions},
    common::Limit,
    formats::{FormatOptions, FormatReader, TrackType, probe::Hint},
    io::{MediaSource, MediaSourceStream},
    meta::MetadataOptions,
};

#[derive(Clone, Copy, Debug)]
pub struct DecodeLimits {
    pub max_file_bytes: u64,
    pub max_packet_bytes: usize,
    pub max_packet_frames: usize,
    pub max_metadata_bytes: usize,
    pub max_duration_seconds: u32,
    pub max_packets: u64,
}
impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 256 * 1024 * 1024,
            max_packet_bytes: 256 * 1024,
            max_packet_frames: 65_536,
            max_metadata_bytes: 64 * 1024,
            max_duration_seconds: 3600,
            max_packets: 1_000_000,
        }
    }
}
impl DecodeLimits {
    fn validate(self) -> Result<()> {
        if self.max_file_bytes == 0
            || self.max_file_bytes > 1024 * 1024 * 1024
            || !(1..=4 * 1024 * 1024).contains(&self.max_packet_bytes)
            || !(1..=65_536).contains(&self.max_packet_frames)
            || self.max_metadata_bytes > 1024 * 1024
            || !(1..=86_400).contains(&self.max_duration_seconds)
            || !(1..=10_000_000).contains(&self.max_packets)
        {
            return Err(failure(
                "audio.decode.limits",
                "Invalid decoder resource limits",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecoderInfo {
    pub format: &'static str,
    pub codec: &'static str,
    pub sample_rate: u32,
    pub channels: u16,
    /// Exact container-declared playable frame count, when available.
    pub total_frames: Option<u64>,
    /// Informational bitrate estimate; never used for trimming, EOF, or seek bounds.
    pub estimated_total_frames: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeProgress {
    Queued {
        frames: usize,
    },
    Backpressure,
    /// One packet was processed without output (another track, codec delay, or seek preroll).
    Skipped,
    Finished,
}
fn failure(code: &'static str, message: &str) -> AudioError {
    AudioError::new(code, message)
}
fn decode_error(e: symphonia::core::errors::Error) -> AudioError {
    let code = match &e {
        symphonia::core::errors::Error::Unsupported(_) => "audio.decode.unsupported",
        _ => "audio.decode.invalid",
    };
    AudioError::new(code, e.to_string())
}
/// Regular files only. Reads are limited to the original file length and check cancellation.
struct LocalSource {
    file: File,
    len: u64,
    position: u64,
    cancel: Arc<AtomicBool>,
    seekable: bool,
}
impl Read for LocalSource {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(io::Error::other("Decoder cancelled"));
        }
        let count = out
            .len()
            .min(32 * 1024)
            .min(self.len.saturating_sub(self.position) as usize);
        let n = self.file.read(&mut out[..count])?;
        self.position += n as u64;
        Ok(n)
    }
}
impl Seek for LocalSource {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(io::Error::other("Decoder cancelled"));
        }
        let position = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::End(n) => i128::from(self.len) + i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
        };
        if position < 0 || position > i128::from(self.len) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Seek outside bounded file",
            ));
        }
        self.position = self.file.seek(SeekFrom::Start(position as u64))?;
        Ok(self.position)
    }
}
impl MediaSource for LocalSource {
    fn is_seekable(&self) -> bool {
        self.seekable
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}
/// A caller-driven decoder retaining at most one packet of interleaved PCM on backpressure.
pub struct AudioDecoder {
    source: File,
    limits: DecodeLimits,
    info: DecoderInfo,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn SymDecoder>,
    track: u32,
    pending: Vec<f32>,
    offset: usize,
    decoded_frames: u64,
    skip_frames: u64,
    packets: u64,
    eof: bool,
    terminal: Option<AudioError>,
    pub(crate) cancel: Arc<AtomicBool>,
}
impl AudioDecoder {
    pub fn open(path: impl AsRef<Path>, limits: DecodeLimits) -> Result<Self> {
        Self::open_inner(path.as_ref(), limits, Arc::new(AtomicBool::new(false)))
    }
    fn open_inner(path: &Path, limits: DecodeLimits, cancel: Arc<AtomicBool>) -> Result<Self> {
        limits.validate()?;
        if cancel.load(Ordering::Acquire) {
            return Err(failure("audio.decode.cancelled", "Decoder cancelled"));
        }
        let before = std::fs::metadata(path)
            .map_err(|e| AudioError::new("audio.decode.io", e.to_string()))?;
        if !before.is_file() || before.len() == 0 || before.len() > limits.max_file_bytes {
            return Err(failure(
                "audio.decode.file_limit",
                "Expected a nonempty bounded regular file",
            ));
        }
        let file =
            File::open(path).map_err(|e| AudioError::new("audio.decode.io", e.to_string()))?;
        Self::from_file(
            file,
            path.extension().and_then(|s| s.to_str()),
            limits,
            cancel,
        )
    }
    fn from_file(
        mut file: File,
        extension: Option<&str>,
        limits: DecodeLimits,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self> {
        if cancel.load(Ordering::Acquire) {
            return Err(failure("audio.decode.cancelled", "Decoder cancelled"));
        }
        let meta = file
            .metadata()
            .map_err(|e| AudioError::new("audio.decode.io", e.to_string()))?;
        if !meta.is_file() || meta.len() == 0 || meta.len() > limits.max_file_bytes {
            return Err(failure(
                "audio.decode.file_limit",
                "Expected a nonempty bounded regular file",
            ));
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|e| AudioError::new("audio.decode.io", e.to_string()))?;
        let retained = file
            .try_clone()
            .map_err(|e| AudioError::new("audio.decode.io", e.to_string()))?;
        let mut format = probe_file(file, meta.len(), extension, limits, cancel.clone(), true)?;
        let initial_frames = format
            .default_track(TrackType::Audio)
            .and_then(|t| t.num_frames);
        if format.format_info().short_name == "mp3" {
            let original_track = audio_signature(&*format);
            // The pinned MP3 reader otherwise estimates a frame count from ~17 frames,
            // then trims packets to that estimate. A forward-only probe prevents both.
            let mut forward = retained
                .try_clone()
                .map_err(|e| AudioError::new("audio.decode.io", e.to_string()))?;
            forward
                .seek(SeekFrom::Start(0))
                .map_err(|e| AudioError::new("audio.decode.io", e.to_string()))?;
            format = probe_file(
                forward,
                meta.len(),
                extension,
                limits,
                cancel.clone(),
                false,
            )?;
            if audio_signature(&*format) != original_track {
                return Err(failure(
                    "audio.decode.changed",
                    "MP3 track changed during bounded re-probe",
                ));
            }
        }
        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| failure("audio.decode.unsupported", "No audio track"))?;
        let parameters = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| failure("audio.decode.unsupported", "No audio codec parameters"))?;
        let rate = parameters
            .sample_rate
            .ok_or_else(|| failure("audio.decode.format", "Unknown sample rate"))?;
        let channels = parameters
            .channels
            .as_ref()
            .map(|c| c.count())
            .ok_or_else(|| failure("audio.decode.format", "Unknown channel layout"))?;
        if !valid_rate(rate) || !matches!(channels, 1 | 2) {
            return Err(failure(
                "audio.decode.format",
                "Only mono/stereo 8-192 kHz audio is supported",
            ));
        }
        if parameters
            .extra_data
            .as_ref()
            .is_some_and(|b| b.len() > limits.max_packet_bytes)
            || parameters
                .max_frames_per_packet
                .is_some_and(|n| n > limits.max_packet_frames as u64)
        {
            return Err(failure(
                "audio.decode.packet_limit",
                "Codec initialization exceeds packet limits",
            ));
        }
        if track
            .num_frames
            .is_some_and(|n| n > u64::from(rate) * u64::from(limits.max_duration_seconds))
        {
            return Err(failure(
                "audio.decode.duration_limit",
                "Declared audio duration exceeds limit",
            ));
        }
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(parameters, &AudioDecoderOptions::default())
            .map_err(decode_error)?;
        // Some readers omit the packet bound until codec initialization (notably FLAC).
        let initialized = decoder.codec_params();
        if initialized
            .max_frames_per_packet
            .is_some_and(|n| n > limits.max_packet_frames as u64)
        {
            return Err(failure(
                "audio.decode.packet_limit",
                "Initialized codec exceeds packet frame limit",
            ));
        }
        if initialized.sample_rate != Some(rate)
            || initialized.channels.as_ref().map(|c| c.count()) != Some(channels)
        {
            return Err(failure(
                "audio.decode.changed",
                "Initialized codec changed the declared output format",
            ));
        }
        let info = DecoderInfo {
            format: format.format_info().short_name,
            codec: decoder.codec_info().short_name,
            sample_rate: rate,
            channels: channels as u16,
            total_frames: track.num_frames,
            estimated_total_frames: if track.num_frames.is_none() {
                initial_frames
            } else {
                None
            },
        };
        let track = track.id;
        Ok(Self {
            source: retained,
            limits,
            info,
            format,
            decoder,
            track,
            pending: Vec::with_capacity(limits.max_packet_frames * channels),
            offset: 0,
            decoded_frames: 0,
            skip_frames: 0,
            packets: 0,
            eof: false,
            terminal: None,
            cancel,
        })
    }
    pub fn info(&self) -> &DecoderInfo {
        &self.info
    }
    /// Cancellation is terminal and also checked by file reads. It never waits for queue space.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    /// Restart decoding and incrementally discard preroll for an exact source-frame seek.
    /// The caller separately obtains a fresh playback generation with `seek_stream`.
    pub(crate) fn max_seek_frame(&self) -> u64 {
        self.info
            .total_frames
            .unwrap_or(u64::MAX)
            .min(u64::from(self.info.sample_rate) * u64::from(self.limits.max_duration_seconds))
    }
    pub fn seek(&mut self, source_frame: u64) -> Result<()> {
        if source_frame
            > u64::from(self.info.sample_rate) * u64::from(self.limits.max_duration_seconds)
            || self.info.total_frames.is_some_and(|n| source_frame > n)
        {
            return Err(failure("audio.decode.seek", "Seek exceeds audio duration"));
        }
        let file = self
            .source
            .try_clone()
            .map_err(|e| AudioError::new("audio.decode.io", e.to_string()))?;
        let mut fresh = match Self::from_file(file, None, self.limits, self.cancel.clone()) {
            Ok(fresh) => fresh,
            Err(error) => {
                self.terminal = Some(error.clone());
                return Err(error);
            }
        };
        if fresh.info != self.info {
            self.terminal = Some(failure(
                "audio.decode.changed",
                "Audio format changed while seeking",
            ));
            return Err(failure(
                "audio.decode.changed",
                "Audio format changed while seeking",
            ));
        }
        fresh.skip_frames = source_frame;
        *self = fresh;
        Ok(())
    }
    /// Processes at most one demux packet, or retries the pending PCM suffix.
    pub fn pump(
        &mut self,
        writer: &mut StreamWriter,
        generation: StreamGeneration,
    ) -> Result<DecodeProgress> {
        let result = if self.cancel.load(Ordering::Acquire) {
            Err(failure("audio.decode.cancelled", "Decoder cancelled"))
        } else if let Some(error) = &self.terminal {
            Err(error.clone())
        } else {
            self.pump_inner(writer, generation)
        };
        let result = if self.cancel.load(Ordering::Acquire) {
            Err(failure("audio.decode.cancelled", "Decoder cancelled"))
        } else {
            result
        };
        if let Err(e) = &result
            && !matches!(e.code, "audio.stream.stale" | "audio.stream.stopped")
        {
            self.terminal = Some(e.clone());
            let _ = writer.fail(generation, e.clone());
        }
        result
    }
    fn pump_inner(
        &mut self,
        writer: &mut StreamWriter,
        generation: StreamGeneration,
    ) -> Result<DecodeProgress> {
        let snapshot = writer.snapshot()?;
        if snapshot.sample_rate != self.info.sample_rate || snapshot.channels != self.info.channels
        {
            return Err(failure(
                "audio.decode.format",
                "Queue format differs from decoder output",
            ));
        }
        if snapshot.generation != generation {
            return Err(failure("audio.stream.stale", "Stale decoder generation"));
        }
        if matches!(snapshot.state, crate::StreamState::Stopped) {
            return Err(failure("audio.stream.stopped", "Playback stopped"));
        }
        if self.offset < self.pending.len() {
            return self.flush(writer, generation);
        }
        if self.eof {
            return Ok(DecodeProgress::Finished);
        }
        if self.packets >= self.limits.max_packets {
            return Err(failure(
                "audio.decode.packet_limit",
                "Packet work budget exhausted",
            ));
        }
        let Some(packet) = self.format.next_packet().map_err(decode_error)? else {
            if self.skip_frames > 0
                || self
                    .info
                    .total_frames
                    .is_some_and(|n| self.decoded_frames < n)
            {
                return Err(failure(
                    "audio.decode.truncated",
                    "Audio ended before its declared frame count",
                ));
            }
            self.eof = true;
            writer.finish(generation)?;
            return Ok(DecodeProgress::Finished);
        };
        self.packets += 1;
        while !self.format.metadata().is_latest() {
            self.format.metadata().pop();
        }
        if packet.data.len() > self.limits.max_packet_bytes {
            return Err(failure(
                "audio.decode.packet_limit",
                "Encoded packet exceeds limit",
            ));
        }
        if packet.track_id != self.track {
            return Ok(DecodeProgress::Skipped);
        }
        let audio = self.decoder.decode(&packet).map_err(decode_error)?;
        let frames = audio.frames();
        if audio.spec().rate() != self.info.sample_rate
            || audio.spec().channels().count() != usize::from(self.info.channels)
        {
            return Err(failure(
                "audio.decode.changed",
                "Midstream format changes are unsupported",
            ));
        }
        if frames > self.limits.max_packet_frames {
            return Err(failure(
                "audio.decode.packet_limit",
                "Decoded packet exceeds frame limit",
            ));
        }
        self.decoded_frames = self
            .decoded_frames
            .checked_add(frames as u64)
            .ok_or_else(|| failure("audio.decode.duration_limit", "Frame count overflow"))?;
        if self.decoded_frames
            > u64::from(self.info.sample_rate) * u64::from(self.limits.max_duration_seconds)
        {
            return Err(failure(
                "audio.decode.duration_limit",
                "Decoded duration exceeds limit",
            ));
        }
        self.pending.resize(audio.samples_interleaved(), 0.);
        audio.copy_to_slice_interleaved(&mut self.pending);
        if self.pending.iter().any(|s| !s.is_finite()) {
            return Err(failure("audio.decode.pcm", "Decoder emitted nonfinite PCM"));
        }
        // Lossy synthesis can overshoot slightly; normalize to the engine PCM contract.
        for sample in &mut self.pending {
            *sample = sample.clamp(-1., 1.);
        }
        let skip = self.skip_frames.min(frames as u64);
        self.skip_frames -= skip;
        self.offset = skip as usize * usize::from(self.info.channels);
        if self.offset == self.pending.len() {
            return Ok(DecodeProgress::Skipped);
        }
        self.flush(writer, generation)
    }
    fn flush(
        &mut self,
        writer: &mut StreamWriter,
        generation: StreamGeneration,
    ) -> Result<DecodeProgress> {
        let count = writer.write(generation, &self.pending[self.offset..])?;
        self.offset += count * usize::from(self.info.channels);
        Ok(if count == 0 {
            DecodeProgress::Backpressure
        } else {
            DecodeProgress::Queued { frames: count }
        })
    }
}

fn probe_file(
    file: File,
    len: u64,
    extension: Option<&str>,
    limits: DecodeLimits,
    cancel: Arc<AtomicBool>,
    seekable: bool,
) -> Result<Box<dyn FormatReader>> {
    let source = LocalSource {
        file,
        len,
        position: 0,
        cancel,
        seekable,
    };
    let stream = MediaSourceStream::new(Box::new(source), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = extension {
        hint.with_extension(extension);
    }
    let metadata = MetadataOptions::default()
        .limit_tag_bytes(Limit::Maximum(limits.max_metadata_bytes))
        .limit_visual_bytes(Limit::Maximum(0));
    symphonia::default::get_probe()
        .probe(&hint, stream, FormatOptions::default(), metadata)
        .map_err(decode_error)
}

fn audio_signature(
    format: &dyn FormatReader,
) -> Option<(
    u32,
    Option<u32>,
    Option<usize>,
    symphonia::core::codecs::audio::AudioCodecId,
)> {
    let track = format.default_track(TrackType::Audio)?;
    let parameters = track.codec_params.as_ref()?.audio()?;
    Some((
        track.id,
        parameters.sample_rate,
        parameters.channels.as_ref().map(|c| c.count()),
        parameters.codec,
    ))
}
