# Synthetic compressed-audio fixtures

These files contain a generated 440 Hz mono sine, not recorded media. They are
project-owned test data under the crate's MIT license. FFmpeg/FFprobe 7.0.2 were
used once to generate them; running Rust tests requires neither executable.
No local machine paths are part of the fixture contract.

```text
ffmpeg -f lavfi -i sine=frequency=440:sample_rate=44100:duration=0.2 -ac 1 -c:a libmp3lame -b:a 96k -write_xing 1 -map_metadata -1 tone.mp3
ffmpeg -f lavfi -i sine=frequency=440:sample_rate=44100:duration=0.2 -ac 1 -c:a libvorbis -q:a 4 -map_metadata -1 tone.ogg
ffmpeg -i tone.mp3 -f f32le -acodec pcm_f32le tone-mp3.f32
ffmpeg -i tone.ogg -af atrim=start_sample=0:end_sample=8820,asetpts=PTS-STARTPTS -f f32le -acodec pcm_f32le tone-vorbis.f32
```

Both references contain 8,820 little-endian f32 mono samples at 44.1 kHz. MP3's
Xing/LAME metadata accounts for encoder delay/padding. The Ogg EOS granule is
8,820; the explicit sample-count trim excludes trailing padding in FFmpeg 7.0.2's
raw output for this short Vorbis clip. Tests compare decoded PCM with tolerance
for different decoder arithmetic and compare exact source-frame seek suffixes.

Additional fixtures are constructed directly in `decoding.rs`: FLAC constant
subframes with CRCs, PCM WAV, and headerless MPEG-1 Layer III empty granules.
The VBR regression has 20 frames at 32 kbit/s followed by 20 at 320 kbit/s, and
the inverse. These are actual compressed bitstreams with 46,080 playable silence
frames. They intentionally omit Xing headers to exercise bitrate estimates.
Malformed/truncated variants are derived in memory from these fixtures.
