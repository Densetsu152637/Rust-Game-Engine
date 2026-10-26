# Desktop audio and player persistence

Audio lives in `engine-audio`; entity attachment, saves and settings live in
`engine-project`. The host owns lifetime, resource paths, the project identifier and
the user-data root; games reach these services through declared asset IDs and safe
slot names. Crate READMEs (`crates/engine-audio/README.md`,
`crates/engine-project/README.md`) hold the detailed contracts.

## Audio (`engine-audio`)

`engine-audio` has no ECS dependency. It owns immutable PCM clips, deterministic
stereo mixing, real device output through CPAL, reusable source handles and ordered
event subscriptions. WAV decoding uses Hound.

- **Clips.** WAV files are bounded to 64 MiB with strict RIFF framing (truncation,
  duplicates and inconsistent PCM rates are rejected). 16-bit integer mono or stereo
  at 8-192 kHz is accepted; `Clip::new` also accepts validated finite PCM in
  `[-1,1]`. Clips are validated once, not on every play.
- **Engines.** `AudioEngine::offline(config)` mixes deterministically without
  hardware; `AudioEngine::device(...)` opens the default CPAL device at the requested
  stereo rate (F32, I16 or U16 output). Live engines reject offline mixing so two
  consumers cannot advance one cursor. `AudioEngine::control()` returns a cloneable
  `AudioControl`.
- **Mixing.** Linear sample interpolation, pitch `1/8` to `8`, continuous loop wrap,
  pause that retains the cursor, and one-shot retirement. Voice, source and observer
  caps bound cue spam. Master, Music, Effects and Dialogue gains multiply the voice
  gain; gains clamp to `[0,1]`, non-finite gain is silence, and output clamps to
  `[-1,1]`.
- **Spatial audio.** Right-handed coordinates with +X right, +Y up and forward -Z.
  Listener forward/up define the right vector. Spatial stereo clips downmix to mono,
  then equal-power panning combines with `None`, `Linear` or `Inverse` distance
  attenuation (silent at and beyond max distance). There is no HRTF, Doppler,
  obstruction or reverb.
- **Sources and events.** `AudioSources` creates reusable sources (`create`),
  controls them (`SourceAction::Play`, pause, resume, stop), reconfigures them while
  keeping the cursor, and subscribes sources to named events with `observe` and
  `notify`. Notification order is registration order; an error stops processing with
  earlier effects retained. Handles are typed and never reused, and dropping a
  controller stops its own voices.
- **Failure policy.** Device failure is sticky, keeps the first diagnostic, clears
  voices and rejects new playback. `device_frames` records actual callback
  submission, which shows output activity but cannot show audibility. The mixer
  uses a mutex and makes no hard-realtime claim.

### Runtime attachment

The desktop host supplies `Runtime::set_audio(AudioControl)` before startup. The
`engine_project::audio` module then offers entity-scoped `source`, `configure`,
`control`, `observe`, `notify` and `listener`. Attachment transforms are resolved
after the native tick; deleting an attached entity removes its sources and
subscriptions, entity teardown drops scoped voices, and stopping the runtime resets
the listener. Audio assets resolve through the confined asset catalog. Collision
trigger events notify `trigger:<entityId>:enter`, `:stay` and `:exit` for both bodies
of each pair. Headless runs advance an offline mixer (800 stereo frames per 1/60 s
tick) and never open a device.

## Player persistence (`engine_project::persistence`)

`PersistenceStore::new(PersistenceOptions)` takes a host-selected root and project
ID. `default_user_data_root(project_id)` returns `%LOCALAPPDATA%\CPPGameEngine` on
Windows, or `$XDG_DATA_HOME/CPPGameEngine` (falling back to
`~/.local/share/CPPGameEngine`) elsewhere, and fails with
`persistence.root.unavailable` when unavailable. The `CPPGameEngine` directory name,
the `content_version` envelope key, the `saves/` and `settings/preferences.json`
layout and the FNV-derived `project-<decimal>` profile ID from `project_id(name)` are
kept from the earlier C++ engine, so existing player data stays in place. Under the
root, each project ID has separate `saves/<slot>.json` and
`settings/preferences.json` files, so settings corruption does not affect
progression and vice versa. `--user-data <directory>` on `engine run` overrides the
root; player data never belongs inside a packaged project.

- **Names.** Slot and project identifiers contain only ASCII letters, digits,
  underscore and hyphen, at most 64 bytes. Paths, traversal, links and junctions
  (including temporary, backup and lock files) are rejected. On macOS, the system
  aliases `/var` and `/tmp` are resolved only when their actual link targets are
  `/private/var` and `/private/tmp`, respectively. Every component of the resolved
  root is then checked; Windows accepts ordinary absolute and canonical verbatim
  disk roots while checking the complete root and every descendant for links and
  junctions. Arbitrary links below these aliases remain rejected,
  including when the requested data root does not yet exist. This confines authored
  content in a trusted user-data directory; it is not a defense against another
  process swapping directory entries.
- **Envelope.** Each document is `{ "schema": N, "content_version": M, "data": {...} }`
  with defaults of schema 1, content version 1, 1 MiB and nesting depth 32 (options
  allow up to 64 MiB and depth 64). Depth is checked before recursive parsing.
  Missing, corrupt, oversized and newer snapshots are distinct coded errors
  (`persistence.missing`, `.corrupt`, `.size.exceeded`, `.depth.exceeded`,
  `.version.newer`).
- **Migration.** A `Migration` callback (`fn(from_schema, from_content, data)`)
  transforms old data; its result is revalidated. A migrated load never rewrites
  disk; pass the callback to `save` to commit it. Newer primary or backup versions
  are never migrated or overwritten.
- **Writes.** A per-slot exclusive file lock serializes writers. The new bytes go to
  a same-directory temporary file, are flushed with `sync_all`, then atomically
  replace the primary (plus a directory sync on Unix). The previous valid primary is
  first committed to `<slot>.json.bak`. A failed replacement leaves a usable old
  primary or backup. Physical power-loss survival still depends on the file system
  and hardware.
- **Recovery.** A corrupt primary is never silently overwritten. `load_backup` is a
  deliberate read-only choice, and `recover_backup` validates the backup, restores
  the primary and keeps the known-good backup; it cannot replace a newer primary.
- **Settings.** `load_settings` and `save_settings` use the same envelope and write
  policy for generic data such as remaps, volume and display preferences.

Named failure stages (`PersistenceStage::Write`, `Flush`, `Replace`) can be injected
through `PersistenceOptions::fail` to test durability paths deterministically.

## Validation

`cargo test -p engine-audio` covers decoding, mixing, state, spatial output,
source/event handling, concurrent control and fault injection. The real-device check
is explicit and requires an output device:

```text
cargo test -p engine-audio native_device_opens_submits_pcm_and_closes -- --ignored --nocapture
```

It requires callback progress, consumed voice PCM, no device diagnostic and clean
shutdown. Audibility, loop seams and physical disconnect behavior need manual
checks. Persistence behavior (envelope and path identity, newer-version protection,
migration, backup recovery, failed replacement, slot locking) is covered by
`crates/engine-project/tests/parity.rs`; audio attachment by
`native_audio_tracks_entity_attachment_and_resets_listener_on_teardown` in the same
file and by the `examples/desktop-2d` tests. See [Testing and CI](testing.md).
