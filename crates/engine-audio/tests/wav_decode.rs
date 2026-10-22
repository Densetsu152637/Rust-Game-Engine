//! WAV decoding policy: accepted container shapes, rejected malformed files, and
//! `Clip` construction limits. All input is synthesized in memory.

mod common;

use common::*;
use engine_audio::{Clip, decode_wav, load_wav};

const INVALID: &str = "audio.wav.invalid";
const UNSUPPORTED: &str = "audio.wav.unsupported";

macro_rules! accepts_layout {
    ($($name:ident: $channels:expr, $rate:expr, $frames:expr;)*) => {$(
        #[test]
        fn $name() {
            let samples: Vec<i16> = (0..$frames * ($channels as usize)).map(|i| (i as i16) * 3 - 5).collect();
            let clip = decode_wav(&wav16($channels, $rate, &samples)).unwrap();
            assert_eq!(clip.sample_rate(), $rate);
            assert_eq!(clip.channels(), $channels);
            assert_eq!(clip.frames(), $frames);
            assert_eq!(clip.samples().len(), samples.len());
            for (decoded, raw) in clip.samples().iter().zip(&samples) {
                assert_eq!(*decoded, f32::from(*raw) / 32768.0);
            }
        }
    )*};
}

accepts_layout! {
    mono_at_lowest_supported_rate: 1u16, 8_000u32, 7usize;
    mono_at_telephone_rate: 1u16, 11_025u32, 5usize;
    mono_at_cd_rate: 1u16, 44_100u32, 9usize;
    stereo_at_engine_rate: 2u16, 48_000u32, 6usize;
    stereo_at_double_rate: 2u16, 96_000u32, 4usize;
    stereo_at_highest_supported_rate: 2u16, 192_000u32, 3usize;
}

macro_rules! rejects_with {
    ($($name:ident: $code:expr, $build:expr;)*) => {$(
        #[test]
        fn $name() {
            let bytes: Vec<u8> = $build;
            let error = decode_wav(&bytes).unwrap_err();
            assert_eq!(error.code, $code, "{error}");
        }
    )*};
}

fn good_fmt() -> Fmt {
    Fmt::pcm16(1, 48_000)
}
fn data4() -> Vec<u8> {
    pcm_bytes(&[100, -100])
}
fn with_fmt(edit: impl FnOnce(&mut Fmt)) -> Vec<u8> {
    let mut fmt = good_fmt();
    edit(&mut fmt);
    wav_with(&fmt, data4())
}
fn bump_riff_size(bytes: &mut [u8], delta: i64) {
    let size = i64::from(u32::from_le_bytes(bytes[4..8].try_into().unwrap())) + delta;
    bytes[4..8].copy_from_slice(&(size as u32).to_le_bytes());
}

rejects_with! {
    rejects_empty_input: INVALID, Vec::new();
    rejects_eleven_byte_input: INVALID, b"RIFF\x03\0\0\0WAV".to_vec();
    rejects_big_endian_riff_magic: INVALID, {
        let mut b = wav16(1, 48_000, &[1, 2]);
        b[..4].copy_from_slice(b"RIFX");
        b
    };
    rejects_wrong_form_type: INVALID, {
        let mut b = wav16(1, 48_000, &[1, 2]);
        b[8..12].copy_from_slice(b"AVI ");
        b
    };
    rejects_riff_size_larger_than_file: INVALID, {
        let mut b = wav16(1, 48_000, &[1, 2]);
        bump_riff_size(&mut b, 1);
        b
    };
    rejects_riff_size_smaller_than_file: INVALID, {
        let mut b = wav16(1, 48_000, &[1, 2]);
        bump_riff_size(&mut b, -1);
        b
    };
    rejects_header_only_riff: INVALID, riff(&[]);
    rejects_data_chunk_longer_than_file: INVALID, {
        let mut b = wav16(1, 48_000, &[1, 2]);
        let at = b.len() - 8;
        b[at..at + 4].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
        b
    };
    rejects_partial_trailing_chunk_header: INVALID, {
        let mut b = wav16(1, 48_000, &[1, 2]);
        b.extend_from_slice(b"abc");
        fix_riff_size(&mut b);
        b
    };
    rejects_chunk_header_without_payload: INVALID, {
        let mut b = wav16(1, 48_000, &[1, 2]);
        b.extend_from_slice(b"LIST\x04\0\0\0");
        fix_riff_size(&mut b);
        b
    };
    rejects_ieee_float_format: UNSUPPORTED, with_fmt(|f| f.format = 3);
    rejects_wave_extensible_tag: UNSUPPORTED, with_fmt(|f| f.format = 0xfffe);
    rejects_adpcm_tag: UNSUPPORTED, with_fmt(|f| f.format = 2);
    rejects_eight_bit_samples: UNSUPPORTED, with_fmt(|f| f.bits = 8);
    rejects_twenty_four_bit_samples: UNSUPPORTED, with_fmt(|f| f.bits = 24);
    rejects_thirty_two_bit_samples: UNSUPPORTED, with_fmt(|f| f.bits = 32);
    rejects_zero_channels: UNSUPPORTED, with_fmt(|f| f.channels = 0);
    rejects_three_channels: UNSUPPORTED, with_fmt(|f| f.channels = 3);
    rejects_surround_channels: UNSUPPORTED, with_fmt(|f| f.channels = 6);
    rejects_zero_rate: UNSUPPORTED, with_fmt(|f| f.rate = 0);
    rejects_rate_just_below_minimum: UNSUPPORTED, with_fmt(|f| f.rate = 7_999);
    rejects_rate_just_above_maximum: UNSUPPORTED, with_fmt(|f| f.rate = 192_001);
    rejects_wrong_byte_rate: INVALID, with_fmt(|f| f.byte_rate += 2);
    rejects_zero_byte_rate: INVALID, with_fmt(|f| f.byte_rate = 0);
    rejects_wrong_block_align: INVALID, with_fmt(|f| f.block_align = 4);
    rejects_zero_block_align: INVALID, with_fmt(|f| f.block_align = 0);
    rejects_empty_data_chunk: INVALID, wav_with(&good_fmt(), Vec::new());
    rejects_mono_data_with_odd_byte_count: INVALID, wav_with(&good_fmt(), vec![1, 2, 3]);
    rejects_stereo_data_with_half_frame: INVALID, wav_with(&Fmt::pcm16(2, 48_000), vec![1, 0, 2, 0, 3, 0]);
    rejects_duplicate_format_chunk: INVALID, riff(&[
        (b"fmt ", good_fmt().bytes()),
        (b"fmt ", good_fmt().bytes()),
        (b"data", data4()),
    ]);
    rejects_duplicate_data_chunk: INVALID, riff(&[
        (b"fmt ", good_fmt().bytes()),
        (b"data", data4()),
        (b"data", data4()),
    ]);
    rejects_missing_format_chunk: INVALID, riff(&[(b"data", data4())]);
    rejects_missing_data_chunk: INVALID, riff(&[(b"fmt ", good_fmt().bytes())]);
    rejects_format_chunk_shorter_than_pcm_header: INVALID, riff(&[
        (b"fmt ", good_fmt().bytes()[..14].to_vec()),
        (b"data", data4()),
    ]);
    rejects_chunk_tags_with_wrong_case: INVALID, riff(&[
        (b"FMT ", good_fmt().bytes()),
        (b"DATA", data4()),
    ]);
}

#[test]
fn rejects_input_larger_than_the_wav_limit_without_parsing_it() {
    let huge = vec![0u8; 64 * 1024 * 1024 + 1];
    assert_eq!(decode_wav(&huge).unwrap_err().code, INVALID);
}

#[test]
fn sample_extremes_map_into_the_unit_range() {
    let clip = decode_wav(&wav16(1, 48_000, &[i16::MIN, -1, 0, 1, i16::MAX])).unwrap();
    assert_eq!(
        clip.samples(),
        &[-1.0, -1.0 / 32768.0, 0.0, 1.0 / 32768.0, 32767.0 / 32768.0]
    );
    assert!(clip.samples().iter().all(|s| (-1.0..1.0).contains(s)));
}

#[test]
fn stereo_samples_stay_interleaved_left_then_right() {
    let clip = decode_wav(&wav16(2, 48_000, &[16384, -16384, 8192, -8192])).unwrap();
    assert_eq!(clip.samples(), &[0.5, -0.5, 0.25, -0.25]);
    assert_eq!(clip.frames(), 2);
}

#[test]
fn odd_sized_chunk_before_data_is_skipped_using_its_pad_byte() {
    let bytes = riff(&[
        (b"fmt ", good_fmt().bytes()),
        (b"junk", vec![9, 9, 9]),
        (b"data", pcm_bytes(&[8192, -8192])),
    ]);
    assert_eq!(decode_wav(&bytes).unwrap().samples(), &[0.25, -0.25]);
}

#[test]
fn metadata_chunk_after_data_is_ignored() {
    let bytes = riff(&[
        (b"fmt ", good_fmt().bytes()),
        (b"data", pcm_bytes(&[4096])),
        (b"LIST", b"INFOISFT\x04\0\0\0test".to_vec()),
    ]);
    assert_eq!(decode_wav(&bytes).unwrap().samples(), &[0.125]);
}

#[test]
fn data_chunk_before_format_chunk_is_accepted() {
    let bytes = riff(&[
        (b"data", pcm_bytes(&[2048, 4096])),
        (b"fmt ", Fmt::pcm16(1, 22_050).bytes()),
    ]);
    let clip = decode_wav(&bytes).unwrap();
    assert_eq!(clip.sample_rate(), 22_050);
    assert_eq!(clip.samples(), &[0.0625, 0.125]);
}

#[test]
fn extended_format_chunk_with_zero_extension_is_accepted() {
    let mut fmt = good_fmt().bytes();
    fmt.extend_from_slice(&[0, 0]);
    let bytes = riff(&[(b"fmt ", fmt), (b"data", pcm_bytes(&[1024, -1024]))]);
    let clip = decode_wav(&bytes).unwrap();
    assert_eq!((clip.channels(), clip.frames()), (1, 2));
}

#[test]
fn one_second_of_audio_keeps_every_frame() {
    let samples: Vec<i16> = (0..48_000).map(|i| (i % 2000) as i16).collect();
    let clip = decode_wav(&wav16(1, 48_000, &samples)).unwrap();
    assert_eq!(clip.frames(), 48_000);
    assert_eq!(clip.samples()[1999], 1999.0 / 32768.0);
    assert_eq!(clip.samples()[2000], 0.0);
}

#[test]
fn decoding_the_same_bytes_twice_is_identical() {
    let bytes = wav16(2, 44_100, &[1, -2, 3, -4, 5, -6]);
    let (a, b) = (decode_wav(&bytes).unwrap(), decode_wav(&bytes).unwrap());
    assert_eq!(a.samples(), b.samples());
    assert_eq!(
        (a.sample_rate(), a.channels()),
        (b.sample_rate(), b.channels())
    );
}

#[test]
fn every_prefix_of_a_stereo_file_is_rejected() {
    let bytes = wav16(2, 48_000, &[10, -10, 20, -20]);
    for end in 0..bytes.len() {
        assert!(decode_wav(&bytes[..end]).is_err(), "prefix {end} decoded");
    }
    assert!(decode_wav(&bytes).is_ok());
}

#[test]
fn single_byte_corruption_never_panics_and_accepted_output_stays_valid() {
    let bytes = wav16(2, 48_000, &[10, -10, 20, -20]);
    for at in 0..bytes.len() {
        for value in [0x00u8, 0x01, 0x7f, 0x80, 0xff] {
            let mut corrupt = bytes.clone();
            corrupt[at] = value;
            if let Ok(clip) = decode_wav(&corrupt) {
                assert!(matches!(clip.channels(), 1 | 2));
                assert!((8_000..=192_000).contains(&clip.sample_rate()));
                assert!(clip.samples().iter().all(|s| (-1.0..=1.0).contains(s)));
                assert_eq!(clip.samples().len() % clip.channels() as usize, 0);
            }
        }
    }
}

// ---- Clip::new -------------------------------------------------------------------------

macro_rules! clip_new_rejects {
    ($($name:ident: $rate:expr, $channels:expr, $samples:expr;)*) => {$(
        #[test]
        fn $name() {
            let samples: Vec<f32> = $samples;
            let error = Clip::new($rate, $channels, samples).unwrap_err();
            assert_eq!(error.code, "audio.clip.invalid");
        }
    )*};
}

clip_new_rejects! {
    clip_rejects_rate_below_minimum: 7_999u32, 1u16, vec![0.0];
    clip_rejects_rate_above_maximum: 192_001u32, 1u16, vec![0.0];
    clip_rejects_zero_rate: 0u32, 1u16, vec![0.0];
    clip_rejects_zero_channels: 48_000u32, 0u16, vec![0.0];
    clip_rejects_three_channels: 48_000u32, 3u16, vec![0.0, 0.0, 0.0];
    clip_rejects_empty_samples: 48_000u32, 1u16, vec![];
    clip_rejects_stereo_with_odd_sample_count: 48_000u32, 2u16, vec![0.0, 0.0, 0.0];
    clip_rejects_nan_sample: 48_000u32, 1u16, vec![0.0, f32::NAN];
    clip_rejects_positive_infinity: 48_000u32, 1u16, vec![f32::INFINITY];
    clip_rejects_negative_infinity: 48_000u32, 1u16, vec![f32::NEG_INFINITY];
    clip_rejects_sample_just_above_one: 48_000u32, 1u16, vec![1.000_000_1];
    clip_rejects_sample_just_below_minus_one: 48_000u32, 1u16, vec![0.0, -1.000_000_1];
}

#[test]
fn clip_accepts_inclusive_unit_range_and_rate_bounds() {
    let low = Clip::new(8_000, 1, vec![-1.0, 1.0]).unwrap();
    assert_eq!((low.sample_rate(), low.frames()), (8_000, 2));
    let high = Clip::new(192_000, 2, vec![1.0, -1.0]).unwrap();
    assert_eq!((high.sample_rate(), high.frames()), (192_000, 1));
}

#[test]
fn clip_accessors_report_what_was_supplied() {
    let clip = Clip::new(44_100, 2, vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6]).unwrap();
    assert_eq!(clip.channels(), 2);
    assert_eq!(clip.frames(), 3);
    assert_eq!(clip.samples(), &[0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);
}

// ---- load_wav --------------------------------------------------------------------------

struct TempFile(std::path::PathBuf);
impl TempFile {
    fn new(name: &str) -> Self {
        Self(std::env::temp_dir().join(format!("engine-audio-{}-{name}", std::process::id())))
    }
}
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn load_wav_reads_and_decodes_a_file() {
    let file = TempFile::new("ok.wav");
    std::fs::write(&file.0, wav16(1, 22_050, &[16384, -16384])).unwrap();
    let clip = load_wav(&file.0).unwrap();
    assert_eq!(clip.sample_rate(), 22_050);
    assert_eq!(clip.samples(), &[0.5, -0.5]);
}

#[test]
fn load_wav_reports_missing_files_as_unreadable() {
    let file = TempFile::new("never-created.wav");
    assert_eq!(load_wav(&file.0).unwrap_err().code, "audio.file.unreadable");
}

#[test]
fn load_wav_reports_directories_as_unreadable() {
    let error = load_wav(std::env::temp_dir()).unwrap_err();
    assert_eq!(error.code, "audio.file.unreadable");
}

#[test]
fn load_wav_surfaces_decode_errors_for_garbage_files() {
    let file = TempFile::new("garbage.wav");
    std::fs::write(&file.0, b"definitely not a wave file").unwrap();
    assert_eq!(load_wav(&file.0).unwrap_err().code, INVALID);
}

#[test]
fn load_wav_rejects_empty_files() {
    let file = TempFile::new("empty.wav");
    std::fs::write(&file.0, b"").unwrap();
    assert_eq!(load_wav(&file.0).unwrap_err().code, INVALID);
}

#[test]
fn load_wav_refuses_files_over_64_mib_by_size() {
    let file = TempFile::new("sparse.wav");
    let handle = std::fs::File::create(&file.0).unwrap();
    handle.set_len(64 * 1024 * 1024 + 1).unwrap();
    drop(handle);
    assert_eq!(load_wav(&file.0).unwrap_err().code, "audio.file.size");
}
