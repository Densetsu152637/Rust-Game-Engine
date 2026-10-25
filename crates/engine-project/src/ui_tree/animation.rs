//! Transitions, keyframe animation and the per-node motion state that drives
//! them. Everything is a pure function of elapsed time: state advances only
//! through [`NodeMotion::step`] with the caller's `dt`, so replays with the
//! same `dt` sequence are bit-identical.
use super::{selector::style_error, *};
use std::collections::BTreeMap;

/// Longest transition or animation time, delay included, in seconds.
pub const MAX_MOTION_SECONDS: f32 = 3600.0;
/// Most iterations of a finite animation.
pub const MAX_ANIMATION_ITERATIONS: u32 = 1000;
/// Most keyframes in one animation.
pub const MAX_KEYFRAMES: usize = 16;
/// Most transition specs in one rule.
pub const MAX_RULE_TRANSITIONS: usize = 8;

/// Timing function mapping linear progress in 0..=1 to eased progress.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    #[default]
    Linear,
    /// Quadratic.
    EaseIn,
    EaseOut,
    EaseInOut,
    CubicIn,
    CubicOut,
    CubicInOut,
    /// CSS `cubic-bezier(x1, y1, x2, y2)`; `x1` and `x2` lie in 0..=1.
    Bezier([f32; 4]),
    /// Jump-end step function with this many steps.
    Steps(u32),
}

impl Easing {
    /// Eased progress for linear progress `t` (clamped to 0..=1).
    pub fn apply(self, t: f32) -> f32 {
        let t = if t.is_finite() {
            t.clamp(0.0, 1.0)
        } else {
            0.0
        };
        match self {
            Self::Linear => t,
            Self::EaseIn => t * t,
            Self::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Self::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
                }
            }
            Self::CubicIn => t * t * t,
            Self::CubicOut => 1.0 - (1.0 - t).powi(3),
            Self::CubicInOut => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
            Self::Bezier([x1, y1, x2, y2]) => {
                let curve = |a: f32, b: f32, s: f32| {
                    let u = 1.0 - s;
                    3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
                };
                // Bisection on the monotonic x(s) curve.
                let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
                for _ in 0..24 {
                    let mid = (lo + hi) / 2.0;
                    if curve(x1, x2, mid) < t {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                curve(y1, y2, (lo + hi) / 2.0)
            }
            Self::Steps(n) => {
                let n = n.max(1) as f32;
                (t * n).floor() / n
            }
        }
    }

    pub(super) fn validate(self) -> Result<()> {
        match self {
            Self::Bezier([x1, y1, x2, y2]) => {
                if ![x1, y1, x2, y2]
                    .iter()
                    .all(|v| v.is_finite() && v.abs() <= 16.0)
                    || !(0.0..=1.0).contains(&x1)
                    || !(0.0..=1.0).contains(&x2)
                {
                    return Err(style_error("Bezier easing x values must lie in 0..=1"));
                }
            }
            Self::Steps(n) if !(1..=64).contains(&n) => {
                return Err(style_error("Step easing needs 1..=64 steps"));
            }
            _ => {}
        }
        Ok(())
    }
}

/// A value of an animatable style property.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UiValue {
    Number(f32),
    Pair([f32; 2]),
    Color([f32; 4]),
}

impl UiValue {
    /// Linear interpolation; values of different shapes snap to `to`.
    pub fn lerp(self, to: Self, t: f32) -> Self {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        match (self, to) {
            (Self::Number(a), Self::Number(b)) => Self::Number(mix(a, b)),
            (Self::Pair(a), Self::Pair(b)) => Self::Pair([mix(a[0], b[0]), mix(a[1], b[1])]),
            (Self::Color(a), Self::Color(b)) => Self::Color([
                mix(a[0], b[0]),
                mix(a[1], b[1]),
                mix(a[2], b[2]),
                mix(a[3], b[3]),
            ]),
            _ => to,
        }
    }
}

/// Style properties that transitions and keyframes can drive.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum UiAnimProperty {
    /// Every property below (transitions only).
    All,
    Background,
    Foreground,
    Accent,
    Opacity,
    Scale,
    /// The post-layout translation `offset`.
    Offset,
    /// Pixel width; changes between other units snap.
    Width,
    /// Pixel height; changes between other units snap.
    Height,
}

impl UiAnimProperty {
    pub(super) const TRACKED: [Self; 8] = [
        Self::Background,
        Self::Foreground,
        Self::Accent,
        Self::Opacity,
        Self::Scale,
        Self::Offset,
        Self::Width,
        Self::Height,
    ];

    /// The property's current value in `style`; `None` when it has no
    /// animatable value (a non-pixel width).
    pub fn read(self, style: &UiStyle) -> Option<UiValue> {
        Some(match self {
            Self::All => return None,
            Self::Background => UiValue::Color(style.background),
            Self::Foreground => UiValue::Color(style.foreground),
            Self::Accent => UiValue::Color(style.accent),
            Self::Opacity => UiValue::Number(style.opacity),
            Self::Scale => UiValue::Number(style.scale),
            Self::Offset => UiValue::Pair(style.offset),
            Self::Width => match style.width {
                UiLength::Pixels(v) => UiValue::Number(v),
                _ => return None,
            },
            Self::Height => match style.height {
                UiLength::Pixels(v) => UiValue::Number(v),
                _ => return None,
            },
        })
    }

    /// Store `value` into `style`; a value of the wrong shape is ignored.
    pub fn write(self, style: &mut UiStyle, value: UiValue) {
        match (self, value) {
            (Self::Background, UiValue::Color(v)) => style.background = v,
            (Self::Foreground, UiValue::Color(v)) => style.foreground = v,
            (Self::Accent, UiValue::Color(v)) => style.accent = v,
            (Self::Opacity, UiValue::Number(v)) => style.opacity = v.clamp(0.0, 1.0),
            (Self::Scale, UiValue::Number(v)) => style.scale = v.clamp(0.0, 16.0),
            (Self::Offset, UiValue::Pair(v)) => style.offset = v,
            (Self::Width, UiValue::Number(v)) => style.width = UiLength::Pixels(v.max(0.0)),
            (Self::Height, UiValue::Number(v)) => style.height = UiLength::Pixels(v.max(0.0)),
            _ => {}
        }
    }
}

fn is_linear(easing: &Easing) -> bool {
    *easing == Easing::Linear
}
fn is_zero_f32(value: &f32) -> bool {
    *value == 0.0
}
fn all_property() -> UiAnimProperty {
    UiAnimProperty::All
}
fn is_all(property: &UiAnimProperty) -> bool {
    *property == UiAnimProperty::All
}

/// Property transition played when the property's target value changes.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct UiTransition {
    #[serde(skip_serializing_if = "is_all")]
    pub property: UiAnimProperty,
    /// Seconds from the end of the delay to the target value.
    pub duration: f32,
    /// Seconds the starting value is held before the transition runs.
    #[serde(skip_serializing_if = "is_zero_f32")]
    pub delay: f32,
    #[serde(skip_serializing_if = "is_linear")]
    pub easing: Easing,
}

impl Default for UiTransition {
    fn default() -> Self {
        Self {
            property: all_property(),
            duration: 0.2,
            delay: 0.0,
            easing: Easing::default(),
        }
    }
}

impl UiTransition {
    pub fn new(property: UiAnimProperty, duration: f32, easing: Easing) -> Self {
        Self {
            property,
            duration,
            delay: 0.0,
            easing,
        }
    }

    /// Value `elapsed` seconds after the transition started: `from` through
    /// the delay, then the eased interpolation, then `to`.
    pub fn sample(&self, from: UiValue, to: UiValue, elapsed: f32) -> UiValue {
        let local = elapsed - self.delay;
        if local <= 0.0 {
            return from;
        }
        if self.duration <= 0.0 || local >= self.duration {
            return to;
        }
        from.lerp(to, self.easing.apply(local / self.duration))
    }

    /// Delay plus duration.
    pub fn total(&self) -> f32 {
        self.delay + self.duration
    }

    fn covers(&self, property: UiAnimProperty) -> bool {
        self.property == UiAnimProperty::All || self.property == property
    }

    pub(super) fn validate(&self) -> Result<()> {
        let ok = self.duration.is_finite()
            && self.delay.is_finite()
            && (0.0..=MAX_MOTION_SECONDS).contains(&self.duration)
            && (0.0..=MAX_MOTION_SECONDS).contains(&self.delay);
        if !ok {
            return Err(style_error(
                "Transition duration or delay outside 0..=3600 s",
            ));
        }
        self.easing.validate()
    }
}

/// One keyframe: the properties it sets at progress `at`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct UiKeyframe {
    pub at: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<[f32; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground: Option<[f32; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<[f32; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<[f32; 2]>,
}

impl UiKeyframe {
    fn value(&self, property: UiAnimProperty) -> Option<UiValue> {
        match property {
            UiAnimProperty::Background => self.background.map(UiValue::Color),
            UiAnimProperty::Foreground => self.foreground.map(UiValue::Color),
            UiAnimProperty::Accent => self.accent.map(UiValue::Color),
            UiAnimProperty::Opacity => self.opacity.map(UiValue::Number),
            UiAnimProperty::Scale => self.scale.map(UiValue::Number),
            UiAnimProperty::Offset => self.offset.map(UiValue::Pair),
            _ => None,
        }
    }
}

/// The animatable properties keyframes can set.
const KEYFRAME_PROPERTIES: [UiAnimProperty; 6] = [
    UiAnimProperty::Background,
    UiAnimProperty::Foreground,
    UiAnimProperty::Accent,
    UiAnimProperty::Opacity,
    UiAnimProperty::Scale,
    UiAnimProperty::Offset,
];

/// A named keyframe timeline. A property missing at 0 or 1 starts or ends at
/// the node's base value.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct UiKeyframes {
    pub frames: Vec<UiKeyframe>,
}

impl UiKeyframes {
    /// Value of every property the frames set, at timeline `progress` in
    /// 0..=1. `base` supplies the value a property holds outside its keyframes.
    pub fn sample(
        &self,
        progress: f32,
        base: &dyn Fn(UiAnimProperty) -> Option<UiValue>,
    ) -> Vec<(UiAnimProperty, UiValue)> {
        let progress = progress.clamp(0.0, 1.0);
        let mut out = vec![];
        for property in KEYFRAME_PROPERTIES {
            let mut points: Vec<(f32, UiValue)> = self
                .frames
                .iter()
                .filter_map(|f| f.value(property).map(|v| (f.at, v)))
                .collect();
            if points.is_empty() {
                continue;
            }
            let Some(base) = base(property) else {
                continue;
            };
            if points[0].0 > 0.0 {
                points.insert(0, (0.0, base));
            }
            if points[points.len() - 1].0 < 1.0 {
                points.push((1.0, base));
            }
            let value = points
                .windows(2)
                .find(|w| progress <= w[1].0)
                .map(|w| {
                    let span = w[1].0 - w[0].0;
                    let t = if span <= 0.0 {
                        1.0
                    } else {
                        (progress - w[0].0) / span
                    };
                    w[0].1.lerp(w[1].1, t)
                })
                .unwrap_or(points[points.len() - 1].1);
            out.push((property, value));
        }
        out
    }

    pub(super) fn validate(&self) -> Result<()> {
        if self.frames.is_empty() || self.frames.len() > MAX_KEYFRAMES {
            return Err(style_error("An animation needs 1..=16 keyframes"));
        }
        let color = |c: &[f32; 4]| c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v));
        let mut last = -1.0_f32;
        for frame in &self.frames {
            let ok = frame.at.is_finite()
                && (0.0..=1.0).contains(&frame.at)
                && frame.at > last
                && [frame.background, frame.foreground, frame.accent]
                    .iter()
                    .flatten()
                    .all(color)
                && frame
                    .opacity
                    .is_none_or(|v| v.is_finite() && (0.0..=1.0).contains(&v))
                && frame
                    .scale
                    .is_none_or(|v| v.is_finite() && (0.0..=16.0).contains(&v))
                && frame
                    .offset
                    .is_none_or(|o| o.iter().all(|v| v.is_finite() && v.abs() <= MAX_UI_EXTENT));
            if !ok {
                return Err(style_error(
                    "Keyframe `at` values must strictly increase within 0..=1 and values be in range",
                ));
            }
            last = frame.at;
        }
        Ok(())
    }
}

/// How many times an animation runs.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum UiIterations {
    Count(u32),
    Infinite(InfiniteTag),
}

/// The literal `"infinite"`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InfiniteTag {
    Infinite,
}

impl UiIterations {
    pub const INFINITE: Self = Self::Infinite(InfiniteTag::Infinite);
    fn count(self) -> Option<u32> {
        match self {
            Self::Count(n) => Some(n),
            Self::Infinite(_) => None,
        }
    }
}

impl Default for UiIterations {
    fn default() -> Self {
        Self::Count(1)
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiAnimationDirection {
    #[default]
    Normal,
    Reverse,
    /// Odd iterations run backwards.
    Alternate,
    AlternateReverse,
}

/// Where an animation is at a given time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UiAnimationPhase {
    /// Still inside the delay.
    Waiting,
    /// Running; the eased timeline progress in 0..=1.
    Running(f32),
    /// All iterations done; the eased progress the last iteration ended on.
    Finished(f32),
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}
fn is_false(value: &bool) -> bool {
    !*value
}

/// An animation playing on a node while its rule matches.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct UiAnimation {
    /// Name of a theme keyframe timeline.
    pub name: String,
    /// Seconds per iteration.
    pub duration: f32,
    #[serde(skip_serializing_if = "is_zero_f32")]
    pub delay: f32,
    #[serde(skip_serializing_if = "is_default")]
    pub iterations: UiIterations,
    #[serde(skip_serializing_if = "is_default")]
    pub direction: UiAnimationDirection,
    /// Applied to the iteration's progress before the keyframe lookup.
    #[serde(skip_serializing_if = "is_linear")]
    pub easing: Easing,
    /// Keep the final frame after the last iteration (CSS `forwards`).
    #[serde(skip_serializing_if = "is_false")]
    pub hold: bool,
    /// Plays even under reduced motion (for example a progress spinner).
    #[serde(skip_serializing_if = "is_false")]
    pub essential: bool,
}

impl UiAnimation {
    pub fn looping(name: impl Into<String>, duration: f32) -> Self {
        Self {
            name: name.into(),
            duration,
            iterations: UiIterations::INFINITE,
            ..Default::default()
        }
    }

    /// Phase `elapsed` seconds after the animation started.
    pub fn phase(&self, elapsed: f64) -> UiAnimationPhase {
        let local = elapsed - f64::from(self.delay);
        if local < 0.0 || self.duration <= 0.0 {
            return UiAnimationPhase::Waiting;
        }
        let duration = f64::from(self.duration);
        let cycles = local / duration;
        let (iteration, fraction, finished) = match self.iterations.count() {
            Some(n) if cycles >= f64::from(n) => (u64::from(n.saturating_sub(1)), 1.0, true),
            _ => {
                let whole = cycles.floor();
                (whole as u64, cycles - whole, false)
            }
        };
        let forward = match self.direction {
            UiAnimationDirection::Normal => true,
            UiAnimationDirection::Reverse => false,
            UiAnimationDirection::Alternate => iteration % 2 == 0,
            UiAnimationDirection::AlternateReverse => iteration % 2 == 1,
        };
        let raw = if forward {
            fraction as f32
        } else {
            1.0 - fraction as f32
        };
        let eased = self.easing.apply(raw);
        if finished {
            UiAnimationPhase::Finished(eased)
        } else {
            UiAnimationPhase::Running(eased)
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        let ok = self.duration.is_finite()
            && self.duration > 0.0
            && self.duration <= MAX_MOTION_SECONDS
            && self.delay.is_finite()
            && (0.0..=MAX_MOTION_SECONDS).contains(&self.delay)
            && self
                .iterations
                .count()
                .is_none_or(|n| (1..=MAX_ANIMATION_ITERATIONS).contains(&n));
        if !ok {
            return Err(style_error(
                "Animation duration must be in (0, 3600] s, delay in 0..=3600 s and iterations in 1..=1000 or infinite",
            ));
        }
        self.easing.validate()
    }
}

#[derive(Clone, Debug)]
struct Tween {
    from: UiValue,
    to: UiValue,
    elapsed: f32,
    spec: UiTransition,
}

#[derive(Clone, Debug)]
struct Running {
    spec: UiAnimation,
    frames: UiKeyframes,
    elapsed: f64,
}

/// Motion state of one node: the last target and shown value of every tracked
/// property, the transitions in flight and the running animation.
#[derive(Clone, Debug, Default)]
pub(super) struct NodeMotion {
    target: BTreeMap<UiAnimProperty, UiValue>,
    shown: BTreeMap<UiAnimProperty, UiValue>,
    tweens: BTreeMap<UiAnimProperty, Tween>,
    animation: Option<Running>,
}

fn spec_for(specs: &[UiTransition], property: UiAnimProperty) -> Option<UiTransition> {
    // A specific property overrides `all`; later specs override earlier ones.
    specs
        .iter()
        .rev()
        .find(|s| s.property == property)
        .or_else(|| {
            specs
                .iter()
                .rev()
                .find(|s| s.property == UiAnimProperty::All)
        })
        .copied()
        .filter(|s| s.covers(property))
}

impl NodeMotion {
    /// Advance `dt` seconds towards the cascaded `style`. Transitions begin on
    /// the step that sees a changed target (shown value unchanged that step)
    /// and advance on later steps. `reduced` makes transitions instant and
    /// drops non-essential animations.
    pub fn step(
        &mut self,
        style: &UiStyle,
        transitions: &[UiTransition],
        animation: Option<(&UiAnimation, &UiKeyframes)>,
        dt: f32,
        reduced: bool,
    ) {
        let dt = if dt.is_finite() {
            dt.clamp(0.0, 1.0)
        } else {
            0.0
        };
        for property in UiAnimProperty::TRACKED {
            let Some(target) = property.read(style) else {
                self.target.remove(&property);
                self.shown.remove(&property);
                self.tweens.remove(&property);
                continue;
            };
            // Advance an in-flight transition first.
            if let Some(tween) = self.tweens.get_mut(&property) {
                tween.elapsed += dt;
                let value = tween.spec.sample(tween.from, tween.to, tween.elapsed);
                let done = tween.elapsed >= tween.spec.total();
                self.shown.insert(property, value);
                if done {
                    self.tweens.remove(&property);
                }
            }
            let changed = self.target.get(&property).is_some_and(|t| *t != target);
            let first = !self.shown.contains_key(&property);
            let spec = spec_for(transitions, property).filter(|s| s.total() > 0.0);
            match spec {
                Some(spec) if !first && !reduced => {
                    if changed {
                        // Start from whatever is on screen now.
                        let from = self.shown.get(&property).copied().unwrap_or(target);
                        self.tweens.insert(
                            property,
                            Tween {
                                from,
                                to: target,
                                elapsed: 0.0,
                                spec,
                            },
                        );
                        self.shown.insert(property, from);
                    } else if !self.tweens.contains_key(&property) {
                        self.shown.insert(property, target);
                    }
                }
                _ => {
                    self.shown.insert(property, target);
                    self.tweens.remove(&property);
                }
            }
            self.target.insert(property, target);
        }
        let wanted = animation.filter(|(spec, _)| !reduced || spec.essential);
        match wanted {
            None => self.animation = None,
            Some((spec, frames)) => match &mut self.animation {
                Some(running) if running.spec == *spec && running.frames == *frames => {
                    running.elapsed += f64::from(dt);
                }
                slot => {
                    *slot = Some(Running {
                        spec: spec.clone(),
                        frames: frames.clone(),
                        elapsed: 0.0,
                    });
                }
            },
        }
    }

    /// `style` with the shown transition values and the animation applied.
    pub fn overlay(&self, style: &mut UiStyle) {
        for (property, value) in &self.shown {
            property.write(style, *value);
        }
        let Some(running) = &self.animation else {
            return;
        };
        let progress = match running.spec.phase(running.elapsed) {
            UiAnimationPhase::Waiting => return,
            UiAnimationPhase::Running(p) => p,
            UiAnimationPhase::Finished(p) if running.spec.hold => p,
            UiAnimationPhase::Finished(_) => return,
        };
        let base = style.clone();
        for (property, value) in running.frames.sample(progress, &|p| p.read(&base)) {
            property.write(style, value);
        }
    }

    /// Whether the node still has motion state worth keeping.
    pub fn idle(&self) -> bool {
        self.tweens.is_empty() && self.animation.is_none()
    }
}

/// Motion state of one root, by node ID.
#[derive(Clone, Debug, Default)]
pub(super) struct RootMotion {
    pub nodes: BTreeMap<String, NodeMotion>,
}
