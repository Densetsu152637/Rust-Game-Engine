use crate::{Clip, ClipEvent, LocalPose, blend_poses};
use anyhow::{Result, ensure};
use std::sync::Arc;
#[derive(Clone, Copy, Debug)]
pub struct Playback {
    pub looping: bool,
    pub speed: f32,
}
impl Default for Playback {
    fn default() -> Self {
        Self {
            looping: true,
            speed: 1.,
        }
    }
}
struct Playing {
    clip: Arc<Clip>,
    time: f32,
    options: Playback,
}
struct Fade {
    from: Vec<LocalPose>,
    elapsed: f32,
    duration: f32,
}
pub struct Animator {
    rest: Vec<LocalPose>,
    pose: Vec<LocalPose>,
    playing: Option<Playing>,
    fade: Option<Fade>,
}
impl Animator {
    pub fn new(rest: Vec<LocalPose>) -> Result<Self> {
        ensure!(
            !rest.is_empty() && rest.len() <= 4096,
            "invalid rest pose size"
        );
        for p in &rest {
            p.validate()?;
        }
        Ok(Self {
            pose: rest.clone(),
            rest,
            playing: None,
            fade: None,
        })
    }
    pub fn pose(&self) -> &[LocalPose] {
        &self.pose
    }
    pub fn play(&mut self, clip: Arc<Clip>, options: Playback) -> Result<()> {
        self.transition(clip, options, 0.)
    }
    /// Crossfades from the currently displayed pose, including interrupted fades.
    pub fn transition(&mut self, clip: Arc<Clip>, options: Playback, duration: f32) -> Result<()> {
        ensure!(
            clip.joint_count() == self.rest.len()
                && options.speed.is_finite()
                && options.speed >= 0.
                && duration.is_finite()
                && duration >= 0.,
            "invalid playback"
        );
        let target = clip.sample(0., options.looping, &self.rest)?;
        self.fade = if duration > 0. {
            Some(Fade {
                from: self.pose.clone(),
                elapsed: 0.,
                duration,
            })
        } else {
            self.pose = target;
            None
        };
        self.playing = Some(Playing {
            clip,
            time: 0.,
            options,
        });
        Ok(())
    }
    pub fn update(&mut self, dt: f32) -> Result<Vec<ClipEvent>> {
        ensure!(
            dt.is_finite() && (0.0..=60.).contains(&dt),
            "invalid animation timestep"
        );
        let Some(playing) = &mut self.playing else {
            return Ok(Vec::new());
        };
        let next = playing.time + dt * playing.options.speed;
        let events = playing
            .clip
            .events_between(playing.time, next, playing.options.looping)?;
        let target = playing
            .clip
            .sample(next, playing.options.looping, &self.rest)?;
        let mut finished = false;
        let pose = if let Some(fade) = &mut self.fade {
            let elapsed = (fade.elapsed + dt).min(fade.duration);
            let pose = blend_poses(&fade.from, &target, elapsed / fade.duration)?;
            fade.elapsed = elapsed;
            finished = elapsed >= fade.duration;
            pose
        } else {
            target
        };
        playing.time = if playing.options.looping {
            next.rem_euclid(playing.clip.duration())
        } else {
            next.min(playing.clip.duration())
        };
        self.pose = pose;
        if finished {
            self.fade = None;
        }
        Ok(events)
    }
}
