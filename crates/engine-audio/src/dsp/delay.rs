//! Fractional delay lines on power-of-two rings (allocated once).

/// Ring buffer of past samples. Delays are measured from the most recently
/// pushed sample: delay 0 is that sample, delay 1 the one before it.
#[derive(Clone, Debug)]
pub(crate) struct DelayLine {
    buf: Vec<f32>,
    mask: usize,
    write: usize,
}

impl DelayLine {
    /// Allocates room for at least `max_delay` samples plus interpolation guard.
    pub(crate) fn new(max_delay: usize) -> Self {
        let len = (max_delay + 4).next_power_of_two();
        Self {
            buf: vec![0.0; len],
            mask: len - 1,
            write: 0,
        }
    }
    /// Largest delay that every read method accepts.
    pub(crate) fn max_delay(&self) -> f64 {
        (self.buf.len() - 4) as f64
    }
    #[inline(always)]
    pub(crate) fn push(&mut self, x: f32) {
        self.write = (self.write + 1) & self.mask;
        self.buf[self.write] = x;
    }
    #[inline(always)]
    fn at(&self, delay: usize) -> f32 {
        self.buf[self.write.wrapping_sub(delay) & self.mask]
    }
    /// Integer delay.
    #[inline(always)]
    pub(crate) fn read(&self, delay: usize) -> f32 {
        self.at(delay)
    }
    /// Cubic Hermite (Catmull-Rom) read. Requires `delay >= 1` so the newer
    /// neighbour already exists; an integer delay returns that sample exactly.
    #[inline(always)]
    pub(crate) fn read_hermite(&self, delay: f64) -> f32 {
        let delay = delay.clamp(1.0, self.max_delay());
        let k = delay as i64 as usize;
        let t = (delay - k as f64) as f32;
        let xm1 = self.at(k - 1);
        let x0 = self.at(k);
        let x1 = self.at(k + 1);
        let x2 = self.at(k + 2);
        let c1 = 0.5 * (x1 - xm1);
        let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
        let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
        ((c3 * t + c2) * t + c1) * t + x0
    }
    /// Linear interpolation read (`delay >= 0`).
    #[inline(always)]
    pub(crate) fn read_linear(&self, delay: f64) -> f32 {
        let delay = delay.clamp(0.0, self.max_delay());
        let k = delay as i64 as usize;
        let t = (delay - k as f64) as f32;
        let a = self.at(k);
        a + (self.at(k + 1) - a) * t
    }
    /// Block form of [`read_hermite`](Self::read_hermite) for a delay that is
    /// constant across the block just pushed: `out[i]` is sample `i` of that
    /// block delayed by `delay` (`out.len()` = block length).
    pub(crate) fn read_hermite_block(&self, delay: f64, out: &mut [f32]) {
        let n = out.len();
        let delay = delay.clamp(1.0, self.max_delay() - n as f64);
        let k = delay as i64 as usize;
        let t = (delay - k as f64) as f32;
        let start = self.write.wrapping_sub(k + n - 1);
        for (i, o) in out.iter_mut().enumerate() {
            let i0 = start.wrapping_add(i);
            let xm1 = self.buf[i0.wrapping_add(1) & self.mask];
            let x0 = self.buf[i0 & self.mask];
            let x1 = self.buf[i0.wrapping_sub(1) & self.mask];
            let x2 = self.buf[i0.wrapping_sub(2) & self.mask];
            let c1 = 0.5 * (x1 - xm1);
            let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
            let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
            *o = ((c3 * t + c2) * t + c1) * t + x0;
        }
    }
    /// Block form of [`read_linear`](Self::read_linear) for a constant delay.
    pub(crate) fn read_linear_block(&self, delay: f64, out: &mut [f32]) {
        let n = out.len();
        let delay = delay.clamp(0.0, self.max_delay() - n as f64);
        let k = delay as i64 as usize;
        let t = (delay - k as f64) as f32;
        let start = self.write.wrapping_sub(k + n - 1);
        for (i, o) in out.iter_mut().enumerate() {
            let i0 = start.wrapping_add(i);
            let a = self.buf[i0 & self.mask];
            let b = self.buf[i0.wrapping_sub(1) & self.mask];
            *o = a + (b - a) * t;
        }
    }
    /// Block read whose delay (relative to each sample) moves linearly from
    /// `first` at the first sample to `last` at the last, in 32.32 fixed point.
    /// `hermite` selects cubic Hermite (delays >= 1) over linear interpolation.
    pub(crate) fn read_sweep(&self, first: f64, last: f64, hermite: bool, out: &mut [f32]) {
        let n = out.len();
        let low = if hermite { 1.0 } else { 0.0 };
        let high = self.max_delay() - n as f64;
        let (first, last) = (first.clamp(low, high), last.clamp(low, high));
        const ONE: f64 = 4_294_967_296.0;
        let slope = if n > 1 {
            (last - first) / (n - 1) as f64
        } else {
            0.0
        };
        // Latest-relative delay of sample i: first + i * slope + (n - 1 - i).
        let mut q = ((first + (n - 1) as f64) * ONE) as i64;
        let step = ((slope - 1.0) * ONE) as i64;
        let mask = self.mask;
        for o in out.iter_mut() {
            let k = (q >> 32) as usize;
            let t = (q & 0xffff_ffff) as f32 * (1.0 / 4_294_967_296.0);
            let i0 = self.write.wrapping_sub(k);
            let x0 = self.buf[i0 & mask];
            let x1 = self.buf[i0.wrapping_sub(1) & mask];
            *o = if hermite {
                let xm1 = self.buf[i0.wrapping_add(1) & mask];
                let x2 = self.buf[i0.wrapping_sub(2) & mask];
                let c1 = 0.5 * (x1 - xm1);
                let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
                let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
                ((c3 * t + c2) * t + c1) * t + x0
            } else {
                x0 + (x1 - x0) * t
            };
            q += step;
        }
    }
    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_reads_are_exact_and_fractional_reads_interpolate() {
        let mut line = DelayLine::new(16);
        for i in 0..20 {
            line.push(i as f32);
        }
        assert_eq!(line.read(0), 19.0);
        assert_eq!(line.read_hermite(3.0), 16.0);
        assert!(
            (line.read_hermite(3.25) - 15.75).abs() < 1e-5,
            "linear ramp is exact"
        );
        assert!((line.read_linear(2.5) - 16.5).abs() < 1e-6);
    }

    #[test]
    fn block_reads_match_per_sample_reads() {
        let mut line = DelayLine::new(64);
        for i in 0..40 {
            line.push(((i * 37) % 11) as f32);
        }
        let n = 8;
        let mut h = [0.0; 8];
        let mut l = [0.0; 8];
        line.read_hermite_block(3.3, &mut h);
        line.read_linear_block(3.3, &mut l);
        for i in 0..n {
            let age = (n - 1 - i) as f64;
            assert!((h[i] - line.read_hermite(3.3 + age)).abs() < 1e-5);
            assert!((l[i] - line.read_linear(3.3 + age)).abs() < 1e-5);
        }
        let mut sh = [0.0; 8];
        let mut sl = [0.0; 8];
        line.read_sweep(3.0, 4.4, true, &mut sh);
        line.read_sweep(3.0, 4.4, false, &mut sl);
        for i in 0..n {
            let d = 3.0 + 0.2 * i as f64 + (n - 1 - i) as f64;
            assert!((sh[i] - line.read_hermite(d)).abs() < 1e-4, "{i}");
            assert!((sl[i] - line.read_linear(d)).abs() < 1e-4, "{i}");
        }
    }
}
