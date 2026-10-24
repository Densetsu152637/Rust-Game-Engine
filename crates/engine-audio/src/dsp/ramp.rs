//! Linear parameter ramps. A ramp only restarts when its target changes, so a
//! target that is re-sent unchanged every block still completes in exactly the
//! requested number of frames.

use super::normalize;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Ramp {
    value: f32,
    target: f32,
    step: f32,
    left: u32,
}

impl Ramp {
    pub(crate) fn new(value: f32) -> Self {
        Self {
            value,
            target: value,
            step: 0.0,
            left: 0,
        }
    }
    /// Starts a linear ramp from the current value when `target` is new.
    pub(crate) fn set(&mut self, target: f32, frames: usize) {
        if target == self.target {
            return;
        }
        self.target = target;
        if frames == 0 {
            self.jump(target);
            return;
        }
        self.step = (target - self.value) / frames as f32;
        self.left = frames.min(u32::MAX as usize) as u32;
    }
    pub(crate) fn jump(&mut self, value: f32) {
        self.value = value;
        self.target = value;
        self.left = 0;
    }
    /// Advances one frame and returns the value for that frame.
    #[inline]
    pub(crate) fn next(&mut self) -> f32 {
        if self.left > 0 {
            self.left -= 1;
            if self.left == 0 {
                self.value = self.target;
            } else {
                self.value += self.step;
            }
        }
        self.value
    }
    pub(crate) fn value(&self) -> f32 {
        self.value
    }
    /// True when the next `frames` values are one straight line.
    pub(crate) fn linear_over(&self, frames: usize) -> bool {
        self.left == 0 || self.left as usize >= frames
    }
    /// Writes the next `out.len()` values (same sequence as [`next`](Self::next)).
    pub(crate) fn fill(&mut self, out: &mut [f32]) {
        let mut i = 0;
        while self.left > 0 && i < out.len() {
            self.left -= 1;
            self.value = if self.left == 0 {
                self.target
            } else {
                self.value + self.step
            };
            out[i] = self.value;
            i += 1;
        }
        out[i..].fill(self.value);
    }
    /// Advances `frames` frames at once.
    pub(crate) fn advance(&mut self, frames: usize) -> f32 {
        if self.left > 0 {
            if frames as u64 >= u64::from(self.left) {
                self.value = self.target;
                self.left = 0;
            } else {
                self.value += self.step * frames as f32;
                self.left -= frames as u32;
            }
        }
        self.value
    }
    pub(crate) fn target(&self) -> f32 {
        self.target
    }
    pub(crate) fn settled(&self) -> bool {
        self.left == 0
    }
}

/// Four ramps advanced together (band or lane gains).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ramp4 {
    value: [f32; 4],
    target: [f32; 4],
    step: [f32; 4],
    left: u32,
}

impl Ramp4 {
    pub(crate) fn new(value: [f32; 4]) -> Self {
        Self {
            value,
            target: value,
            step: [0.0; 4],
            left: 0,
        }
    }
    pub(crate) fn set(&mut self, target: [f32; 4], frames: usize) {
        if target == self.target {
            return;
        }
        self.target = target;
        if frames == 0 {
            self.jump(target);
            return;
        }
        for i in 0..4 {
            self.step[i] = (target[i] - self.value[i]) / frames as f32;
        }
        self.left = frames.min(u32::MAX as usize) as u32;
    }
    pub(crate) fn jump(&mut self, value: [f32; 4]) {
        self.value = value;
        self.target = value;
        self.left = 0;
    }
    #[inline]
    pub(crate) fn next(&mut self) -> [f32; 4] {
        if self.left > 0 {
            self.left -= 1;
            if self.left == 0 {
                self.value = self.target;
            } else {
                for i in 0..4 {
                    self.value[i] += self.step[i];
                }
            }
        }
        self.value
    }
    pub(crate) fn target(&self) -> [f32; 4] {
        self.target
    }
}

/// Direction interpolated (then renormalised) from one unit vector to another.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DirRamp {
    from: [f64; 3],
    to: [f64; 3],
    pos: usize,
    len: usize,
}

impl DirRamp {
    pub(crate) fn new(dir: [f64; 3]) -> Self {
        Self {
            from: dir,
            to: dir,
            pos: 0,
            len: 0,
        }
    }
    /// Direction `ahead` frames from now.
    pub(crate) fn at(&self, ahead: usize) -> [f64; 3] {
        if self.len == 0 || self.pos + ahead >= self.len {
            return self.to;
        }
        let t = (self.pos + ahead) as f64 / self.len as f64;
        let mixed = std::array::from_fn(|i| self.from[i] + (self.to[i] - self.from[i]) * t);
        normalize(mixed).unwrap_or(self.to)
    }
    pub(crate) fn set(&mut self, to: [f64; 3], frames: usize) {
        if to == self.to {
            return;
        }
        self.from = self.at(0);
        self.to = to;
        self.pos = 0;
        self.len = frames;
    }
    pub(crate) fn advance(&mut self, frames: usize) {
        self.pos = (self.pos + frames).min(self.len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_is_linear_and_lands_exactly() {
        let mut r = Ramp::new(1.0);
        r.set(0.1, 4);
        let values: Vec<f32> = (0..6).map(|_| r.next()).collect();
        assert!((values[0] - 0.775).abs() < 1e-6);
        assert_eq!(values[3], 0.1);
        assert_eq!(values[5], 0.1);
        // Re-sending the same target does not restart the ramp.
        r.set(0.1, 4);
        assert!(r.settled());
    }

    #[test]
    fn dir_ramp_interpolates_on_the_sphere() {
        let mut d = DirRamp::new([1.0, 0.0, 0.0]);
        d.set([0.0, 1.0, 0.0], 10);
        let mid = d.at(5);
        assert!((mid[0] - mid[1]).abs() < 1e-12);
        assert!((super::super::length(mid) - 1.0).abs() < 1e-12);
        d.advance(20);
        assert_eq!(d.at(0), [0.0, 1.0, 0.0]);
    }
}
