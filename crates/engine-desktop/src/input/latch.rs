//! Turning window input into per-tick action frames when redraws and ticks no
//! longer coincide.
//!
//! A redraw may run zero, one or several ticks. Input is sampled by level (the
//! set of keys down) once per tick and the action edges (`pressed`, `released`)
//! come from comparing consecutive ticks, which already gives the right answer
//! for a key that stays down across redraws with no tick and for several ticks
//! in one redraw (the first sees the press, the rest see it held). What level
//! sampling alone loses is a tap that begins and ends between two ticks; the
//! [`PressLatch`] remembers such presses until a tick has seen them.

use engine_project::input::{ActionFrame, ActionMapper, RawInput, merge_input};
use std::collections::BTreeSet;

/// Presses that happened since the last tick.
#[derive(Debug, Default)]
pub(crate) struct PressLatch {
    pending: BTreeSet<String>,
}

impl PressLatch {
    /// Remember every token that is down in `after` but was not in `before`.
    pub fn note_presses(&mut self, before: &BTreeSet<String>, after: &BTreeSet<String>) {
        self.pending.extend(after.difference(before).cloned());
    }

    /// Forget pending presses (focus lost: held keys are released, not tapped).
    pub fn clear(&mut self) {
        self.pending.clear();
    }

    /// Consume the pending presses and return those no longer down: taps the
    /// next tick must still see as one press and release.
    pub fn take_taps(&mut self, down: &BTreeSet<String>) -> BTreeSet<String> {
        std::mem::take(&mut self.pending)
            .difference(down)
            .cloned()
            .collect()
    }
}

/// Produce the action frame for one tick from the window's current input.
///
/// `injected` is the scripted action pulse for this tick (empty outside a
/// scripted run); `previous` carries the held set between ticks for edge
/// detection. The wheel delta is consumed by the first tick that samples it.
pub(crate) fn sample_tick(
    mapper: &mut ActionMapper,
    raw: &mut RawInput,
    latch: &mut PressLatch,
    injected: &BTreeSet<String>,
    previous: &mut BTreeSet<String>,
) -> ActionFrame {
    let taps = latch.take_taps(&raw.down);
    raw.down.extend(taps.iter().cloned());
    let physical = mapper.sample(raw);
    for token in &taps {
        raw.down.remove(token);
    }
    raw.wheel = [0.0; 2];
    merge_input(&physical, injected, previous)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct Rig {
        mapper: ActionMapper,
        raw: RawInput,
        latch: PressLatch,
        previous: BTreeSet<String>,
    }

    impl Rig {
        fn new() -> Self {
            let bindings = BTreeMap::from([
                ("jump".to_string(), vec!["Key:Space".to_string()]),
                ("fire".to_string(), vec!["Mouse:Left".to_string()]),
            ]);
            Self {
                mapper: ActionMapper::new(bindings).unwrap(),
                raw: RawInput::default(),
                latch: PressLatch::default(),
                previous: BTreeSet::new(),
            }
        }
        fn set(&mut self, token: &str, down: bool) {
            let before = self.raw.down.clone();
            if down {
                self.raw.down.insert(token.into());
            } else {
                self.raw.down.remove(token);
            }
            self.latch.note_presses(&before, &self.raw.down);
        }
        fn tick(&mut self) -> ActionFrame {
            sample_tick(
                &mut self.mapper,
                &mut self.raw,
                &mut self.latch,
                &BTreeSet::new(),
                &mut self.previous,
            )
        }
        /// A redraw that runs `ticks` ticks, returning each tick's frame.
        fn frame(&mut self, ticks: usize) -> Vec<ActionFrame> {
            (0..ticks).map(|_| self.tick()).collect()
        }
    }

    fn pressed(frames: &[ActionFrame], action: &str) -> usize {
        frames.iter().filter(|f| f.pressed(action)).count()
    }
    fn released(frames: &[ActionFrame], action: &str) -> usize {
        frames.iter().filter(|f| f.released(action)).count()
    }

    #[test]
    fn a_press_seen_by_the_first_of_several_ticks_in_one_frame_is_not_duplicated() {
        let mut rig = Rig::new();
        rig.set("Key:Space", true);
        let frames = rig.frame(4);
        assert_eq!(pressed(&frames, "jump"), 1);
        assert!(frames[0].pressed("jump"));
        assert!(frames[1..].iter().all(|f| f.held("jump")));
        assert_eq!(released(&frames, "jump"), 0);
    }

    #[test]
    fn a_press_waits_through_frames_with_no_tick_and_is_seen_once() {
        let mut rig = Rig::new();
        // Three redraws pass with no tick due; the key goes down during them.
        rig.set("Key:Space", true);
        assert!(rig.frame(0).is_empty());
        assert!(rig.frame(0).is_empty());
        let frames = rig.frame(1);
        assert_eq!(pressed(&frames, "jump"), 1);
        assert_eq!(pressed(&rig.frame(1), "jump"), 0, "still held, not pressed");
    }

    #[test]
    fn a_held_key_across_zero_and_many_tick_frames_presses_once_and_releases_once() {
        let mut rig = Rig::new();
        let mut all = Vec::new();
        rig.set("Key:Space", true);
        for ticks in [0, 1, 0, 0, 3, 0, 2] {
            all.extend(rig.frame(ticks));
        }
        rig.set("Key:Space", false);
        all.extend(rig.frame(0));
        all.extend(rig.frame(2));
        assert_eq!(pressed(&all, "jump"), 1);
        assert_eq!(released(&all, "jump"), 1);
    }

    #[test]
    fn a_tap_that_starts_and_ends_between_two_ticks_is_one_press_and_one_release() {
        let mut rig = Rig::new();
        rig.frame(1);
        rig.set("Key:Space", true);
        rig.set("Key:Space", false);
        // Level sampling alone would see nothing; the latch keeps the press.
        let first = rig.tick();
        assert!(first.pressed("jump") && first.held("jump"));
        let second = rig.tick();
        assert!(second.released("jump") && !second.held("jump"));
        assert!(!rig.tick().released("jump"));
    }

    #[test]
    fn a_tap_in_a_multi_tick_frame_is_seen_by_the_first_tick_only() {
        let mut rig = Rig::new();
        rig.set("Key:Space", true);
        rig.set("Key:Space", false);
        let frames = rig.frame(3);
        assert_eq!(pressed(&frames, "jump"), 1);
        assert_eq!(released(&frames, "jump"), 1);
        assert!(frames[0].pressed("jump") && frames[1].released("jump"));
    }

    #[test]
    fn repeated_down_events_for_a_held_key_do_not_re_latch_it() {
        let mut rig = Rig::new();
        rig.set("Key:Space", true);
        rig.frame(1);
        rig.set("Key:Space", true); // OS key repeat: already down
        let frames = rig.frame(2);
        assert_eq!(pressed(&frames, "jump"), 0);
        assert_eq!(released(&frames, "jump"), 0);
    }

    #[test]
    fn two_taps_between_ticks_collapse_into_one_press() {
        let mut rig = Rig::new();
        for _ in 0..2 {
            rig.set("Key:Space", true);
            rig.set("Key:Space", false);
        }
        assert_eq!(pressed(&rig.frame(2), "jump"), 1);
    }

    #[test]
    fn taps_of_different_buttons_are_each_seen() {
        let mut rig = Rig::new();
        rig.set("Key:Space", true);
        rig.set("Key:Space", false);
        rig.set("Mouse:Left", true);
        rig.set("Mouse:Left", false);
        let frames = rig.frame(2);
        assert_eq!(pressed(&frames, "jump"), 1);
        assert_eq!(pressed(&frames, "fire"), 1);
    }

    #[test]
    fn clearing_the_latch_drops_pending_taps() {
        let mut rig = Rig::new();
        rig.set("Key:Space", true);
        rig.set("Key:Space", false);
        rig.latch.clear();
        let frames = rig.frame(2);
        assert_eq!(pressed(&frames, "jump"), 0);
    }

    #[test]
    fn the_wheel_delta_is_consumed_by_the_first_tick() {
        let mut rig = Rig::new();
        rig.raw.wheel = [0.0, 2.0];
        let first = rig.tick();
        assert_eq!(first.wheel, [0.0, 2.0]);
        assert_eq!(rig.tick().wheel, [0.0; 2]);
    }

    #[test]
    fn injected_actions_stay_one_tick_pulses_across_a_multi_tick_frame() {
        let mut rig = Rig::new();
        let pulse = BTreeSet::from(["jump".to_string()]);
        let none = BTreeSet::new();
        let mut frames = Vec::new();
        for injected in [&none, &pulse, &none, &none] {
            frames.push(sample_tick(
                &mut rig.mapper,
                &mut rig.raw,
                &mut rig.latch,
                injected,
                &mut rig.previous,
            ));
        }
        assert_eq!(pressed(&frames, "jump"), 1);
        assert!(frames[1].pressed("jump"));
        assert_eq!(released(&frames, "jump"), 1);
        assert!(frames[2].released("jump"));
    }

    #[test]
    fn note_presses_only_remembers_new_tokens() {
        let mut latch = PressLatch::default();
        let before = BTreeSet::from(["a".to_string()]);
        let after = BTreeSet::from(["a".to_string(), "b".to_string()]);
        latch.note_presses(&before, &after);
        assert_eq!(
            latch.take_taps(&BTreeSet::new()),
            BTreeSet::from(["b".to_string()])
        );
        assert!(latch.take_taps(&BTreeSet::new()).is_empty());
        latch.note_presses(&before, &after);
        assert!(
            latch.take_taps(&after).is_empty(),
            "still down: not a tap, the level shows it"
        );
    }
}
