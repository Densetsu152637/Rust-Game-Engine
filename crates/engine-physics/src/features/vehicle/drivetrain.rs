//! Engine, gearbox and differential descriptors (docs/physics-vehicles.md#drivetrain).
//! Pure maths, no Rapier types.
use crate::{Error, Result};

/// Most torque curve samples and forward gears accepted.
pub const MAX_TORQUE_SAMPLES: usize = 64;
pub const MAX_GEARS: usize = 12;
/// Radians per second to revolutions per minute.
pub const RAD_S_TO_RPM: f32 = 60. / std::f32::consts::TAU;

/// Engine torque and speed limits.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineDesc {
    /// `(rpm, torque in N m)` samples at full throttle, strictly ascending in rpm (at least 2).
    /// Torque is interpolated linearly and held flat outside the sampled range.
    pub torque_curve: Vec<[f32; 2]>,
    pub idle_rpm: f32,
    /// Rev limiter: torque ramps to zero over the last 3% below this speed.
    pub max_rpm: f32,
    /// Closed-throttle drag torque in N m at `max_rpm`, scaled linearly with rpm.
    pub engine_brake: f32,
}
impl Default for EngineDesc {
    fn default() -> Self {
        Self {
            torque_curve: vec![
                [1000., 180.],
                [2500., 260.],
                [4000., 300.],
                [5500., 270.],
                [6500., 220.],
            ],
            idle_rpm: 900.,
            max_rpm: 6500.,
            engine_brake: 60.,
        }
    }
}
impl EngineDesc {
    pub fn validate(self) -> Result<Self> {
        let curve_ok = (2..=MAX_TORQUE_SAMPLES).contains(&self.torque_curve.len())
            && self.torque_curve.iter().all(|s| {
                s[0].is_finite()
                    && s[1].is_finite()
                    && (0. ..=1e5).contains(&s[0])
                    && s[1].abs() <= 1e5
            })
            && self.torque_curve.windows(2).all(|w| w[0][0] < w[1][0]);
        let ok = curve_ok
            && (100. ..=20_000.).contains(&self.idle_rpm)
            && (200. ..=50_000.).contains(&self.max_rpm)
            && self.idle_rpm < self.max_rpm
            && (0. ..=1e5).contains(&self.engine_brake);
        if !ok {
            return Err(Error("invalid engine settings"));
        }
        Ok(self)
    }
    /// Full-throttle torque at `rpm` (N m), interpolated from the curve.
    pub fn torque_at(&self, rpm: f32) -> f32 {
        let curve = &self.torque_curve;
        let (first, last) = (curve[0], curve[curve.len() - 1]);
        if rpm <= first[0] {
            return first[1];
        }
        if rpm >= last[0] {
            return last[1];
        }
        let index = curve.partition_point(|s| s[0] <= rpm);
        let (a, b) = (curve[index - 1], curve[index]);
        a[1] + (b[1] - a[1]) * (rpm - a[0]) / (b[0] - a[0])
    }
    /// Multiplier (0..=1) that fades the torque out at the rev limiter.
    pub fn limiter(&self, rpm: f32) -> f32 {
        let start = self.max_rpm * 0.97;
        ((self.max_rpm - rpm) / (self.max_rpm - start)).clamp(0., 1.)
    }
}

/// Who changes gear.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GearboxMode {
    /// Shifts by rpm thresholds and selects reverse/forward from the pedals at a standstill.
    #[default]
    Automatic,
    /// Shifts only on request (`shift_up`, `shift_down`, `set_gear`).
    Manual,
}

/// Gear ratios. The gear number is `-1` reverse, `0` neutral and `1..=forward.len()` forward.
#[derive(Clone, Debug, PartialEq)]
pub struct GearboxDesc {
    /// Forward ratios (engine revolutions per gearbox output revolution), each `0.1..=30`.
    pub forward: Vec<f32>,
    /// Reverse ratio as a positive magnitude (`0.1..=30`).
    pub reverse: f32,
    /// Final drive ratio (`0.1..=30`).
    pub final_drive: f32,
    /// Mechanical efficiency of the whole driveline (`0.1..=1`).
    pub efficiency: f32,
    pub mode: GearboxMode,
    /// Automatic: shift up at or above this rpm.
    pub up_shift_rpm: f32,
    /// Automatic: shift down at or below this rpm.
    pub down_shift_rpm: f32,
    /// Seconds of torque cut per shift (`0..=5`).
    pub shift_time: f32,
}
impl Default for GearboxDesc {
    fn default() -> Self {
        Self {
            forward: vec![3.6, 2.2, 1.5, 1.1, 0.85],
            reverse: 3.4,
            final_drive: 3.7,
            efficiency: 0.9,
            mode: GearboxMode::Automatic,
            up_shift_rpm: 6000.,
            down_shift_rpm: 2800.,
            shift_time: 0.2,
        }
    }
}
impl GearboxDesc {
    pub fn validate(self) -> Result<Self> {
        let ratio = |r: f32| r.is_finite() && (0.1..=30.).contains(&r);
        let ok = (1..=MAX_GEARS).contains(&self.forward.len())
            && self.forward.iter().all(|&r| ratio(r))
            && ratio(self.reverse)
            && ratio(self.final_drive)
            && self.efficiency.is_finite()
            && (0.1..=1.).contains(&self.efficiency)
            && self.up_shift_rpm.is_finite()
            && self.down_shift_rpm.is_finite()
            && (0. ..=50_000.).contains(&self.down_shift_rpm)
            && self.down_shift_rpm < self.up_shift_rpm
            && self.up_shift_rpm <= 50_000.
            && self.shift_time.is_finite()
            && (0. ..=5.).contains(&self.shift_time);
        if !ok {
            return Err(Error("invalid gearbox settings"));
        }
        Ok(self)
    }
    /// Highest forward gear.
    pub fn top_gear(&self) -> i32 {
        self.forward.len() as i32
    }
    /// Signed overall ratio from engine to wheels: negative in reverse, zero in neutral.
    pub fn overall_ratio(&self, gear: i32) -> f32 {
        match gear {
            0 => 0.,
            g if g < 0 => -self.reverse * self.final_drive,
            g => self
                .forward
                .get(g as usize - 1)
                .map_or(0., |r| r * self.final_drive),
        }
    }
    pub fn valid_gear(&self, gear: i32) -> bool {
        (-1..=self.top_gear()).contains(&gear)
    }
}

/// How drive torque is split between the driven wheels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum DifferentialDesc {
    /// Equal torque to every driven wheel; the unloaded wheel spins (see the split-mu test).
    #[default]
    Open,
    /// All driven wheels turn at one speed (spool).
    Locked,
    /// Open differential plus a clutch pack that moves torque from the faster wheel to the
    /// slower: `bias = preload + viscous * |omega - mean|`, limited to `max_torque`.
    LimitedSlip {
        /// Torque in N m always transferred once the wheels differ in speed (`0..=1e5`).
        preload: f32,
        /// Extra N m per rad/s of speed difference (`0..=1e5`).
        viscous: f32,
        /// Largest transferred torque in N m (`0..=1e5`).
        max_torque: f32,
    },
}
impl DifferentialDesc {
    pub fn validate(self) -> Result<Self> {
        let ok = match self {
            Self::LimitedSlip {
                preload,
                viscous,
                max_torque,
            } => [preload, viscous, max_torque]
                .iter()
                .all(|v| (0. ..=1e5).contains(v)),
            _ => true,
        };
        if !ok {
            return Err(Error("invalid differential settings"));
        }
        Ok(self)
    }
}

/// Splits `total` N m over `speeds` (wheel speeds of the driven wheels) for the open and
/// limited-slip differentials. A locked differential is solved by the caller, which gives all
/// wheels one speed; here it behaves like an open one.
pub fn split_torque(diff: DifferentialDesc, total: f32, speeds: &[f32]) -> Vec<f32> {
    let count = speeds.len().max(1) as f32;
    let share = total / count;
    let mut out = vec![share; speeds.len()];
    if let DifferentialDesc::LimitedSlip {
        preload,
        viscous,
        max_torque,
    } = diff
    {
        let mean = speeds.iter().sum::<f32>() / count;
        let mut bias: Vec<f32> = speeds
            .iter()
            .map(|&w| {
                let delta = w - mean;
                let magnitude =
                    (preload * (delta.abs() * 4.).min(1.) + viscous * delta.abs()).min(max_torque);
                magnitude.copysign(delta)
            })
            .collect();
        let average = bias.iter().sum::<f32>() / count;
        for (torque, bias) in out.iter_mut().zip(bias.iter_mut()) {
            *torque -= *bias - average;
        }
    }
    out
}
