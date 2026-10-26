//! Perception configuration: senses, teams and affiliation filters.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Entity identity as the host numbers it (`Entity::to_bits` in `engine-project`).
pub type Id = u64;
/// Subject of stimuli that have no source entity (an anonymous noise).
pub const ANONYMOUS: Id = u64::MAX;
/// Longest time a stimulus is remembered, seconds.
pub const MAX_MEMORY_SECONDS: f32 = 3600.0;
/// Largest sensing range or noise range, world units.
pub const MAX_RANGE: f32 = 100_000.0;

/// The channel a stimulus arrived through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sense {
    Sight,
    Hearing,
    Damage,
}

impl Sense {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sight => "sight",
            Self::Hearing => "hearing",
            Self::Damage => "damage",
        }
    }
}

/// How a perceiver's team relates to a stimulus' team.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Affiliation {
    Hostile,
    Neutral,
    Friendly,
}

impl Affiliation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hostile => "hostile",
            Self::Neutral => "neutral",
            Self::Friendly => "friendly",
        }
    }
}

/// Which affiliations a sense reacts to. Serialized as a list of affiliations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Vec<Affiliation>", into = "Vec<Affiliation>")]
pub struct AffiliationFilter {
    pub hostile: bool,
    pub neutral: bool,
    pub friendly: bool,
}

impl AffiliationFilter {
    /// Only hostile stimuli (the default of every sense).
    pub const HOSTILE: Self = Self {
        hostile: true,
        neutral: false,
        friendly: false,
    };
    pub const ALL: Self = Self {
        hostile: true,
        neutral: true,
        friendly: true,
    };
    pub fn contains(self, affiliation: Affiliation) -> bool {
        match affiliation {
            Affiliation::Hostile => self.hostile,
            Affiliation::Neutral => self.neutral,
            Affiliation::Friendly => self.friendly,
        }
    }
}

impl From<Vec<Affiliation>> for AffiliationFilter {
    fn from(list: Vec<Affiliation>) -> Self {
        Self {
            hostile: list.contains(&Affiliation::Hostile),
            neutral: list.contains(&Affiliation::Neutral),
            friendly: list.contains(&Affiliation::Friendly),
        }
    }
}

impl From<AffiliationFilter> for Vec<Affiliation> {
    fn from(filter: AffiliationFilter) -> Self {
        [
            Affiliation::Hostile,
            Affiliation::Neutral,
            Affiliation::Friendly,
        ]
        .into_iter()
        .filter(|a| filter.contains(*a))
        .collect()
    }
}

impl Default for AffiliationFilter {
    fn default() -> Self {
        Self::HOSTILE
    }
}

/// Team relations. Team 0 means "no team". Equal non-zero teams are friendly; a
/// pair involving team 0, or a pair without an explicit relation, gets
/// `unaffiliated` respectively `default_affiliation`.
#[derive(Clone, Debug, PartialEq)]
pub struct TeamTable {
    /// Relation of two different non-zero teams without an entry.
    pub default_affiliation: Affiliation,
    /// Relation of any pair that involves team 0 (without an entry).
    pub unaffiliated: Affiliation,
    relations: BTreeMap<(u32, u32), Affiliation>,
}

impl Default for TeamTable {
    fn default() -> Self {
        Self {
            default_affiliation: Affiliation::Hostile,
            unaffiliated: Affiliation::Neutral,
            relations: BTreeMap::new(),
        }
    }
}

impl TeamTable {
    /// A table without explicit relations.
    pub fn with_defaults(default_affiliation: Affiliation, unaffiliated: Affiliation) -> Self {
        Self {
            default_affiliation,
            unaffiliated,
            relations: BTreeMap::new(),
        }
    }
    /// Sets the (symmetric) relation of two teams.
    pub fn set(&mut self, a: u32, b: u32, affiliation: Affiliation) {
        self.relations.insert((a.min(b), a.max(b)), affiliation);
    }
    pub fn len(&self) -> usize {
        self.relations.len()
    }
    pub fn is_empty(&self) -> bool {
        self.relations.is_empty()
    }
    /// How `a` sees `b`.
    pub fn affiliation(&self, a: u32, b: u32) -> Affiliation {
        if let Some(found) = self.relations.get(&(a.min(b), a.max(b))) {
            return *found;
        }
        if a == 0 || b == 0 {
            self.unaffiliated
        } else if a == b {
            Affiliation::Friendly
        } else {
            self.default_affiliation
        }
    }
}

/// Cone sight. A stimulus is acquired inside `range` and `half_angle`, and kept
/// while it stays inside `lose_range` and `peripheral_half_angle`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SightConfig {
    pub range: f32,
    /// Range at which an already sensed target is lost; at least `range`.
    pub lose_range: f32,
    /// Degrees from the forward axis.
    pub half_angle: f32,
    /// Degrees; at least `half_angle`.
    pub peripheral_half_angle: f32,
    /// Eye height above the perceiver position (world +Y).
    pub height_offset: f32,
    /// Seconds a lost target is remembered.
    pub max_age: f32,
    pub affiliations: AffiliationFilter,
}

impl Default for SightConfig {
    fn default() -> Self {
        Self {
            range: 20.0,
            lose_range: 24.0,
            half_angle: 45.0,
            peripheral_half_angle: 60.0,
            height_offset: 0.0,
            max_age: 5.0,
            affiliations: AffiliationFilter::HOSTILE,
        }
    }
}

fn check_range(name: &str, value: f32) -> Result<(), String> {
    if value.is_finite() && value > 0.0 && value <= MAX_RANGE {
        Ok(())
    } else {
        Err(format!("{name} must be in (0, {MAX_RANGE}]"))
    }
}
fn check_age(value: f32) -> Result<(), String> {
    if value.is_finite() && (0.0..=MAX_MEMORY_SECONDS).contains(&value) {
        Ok(())
    } else {
        Err(format!("max_age must be 0 to {MAX_MEMORY_SECONDS} seconds"))
    }
}
fn check_angle(name: &str, value: f32) -> Result<(), String> {
    if value.is_finite() && value > 0.0 && value <= 180.0 {
        Ok(())
    } else {
        Err(format!("{name} must be in (0, 180] degrees"))
    }
}

impl SightConfig {
    /// # Errors
    /// A bound that is violated, with a message naming the field.
    pub fn validate(&self) -> Result<(), String> {
        check_range("sight.range", self.range)?;
        check_range("sight.lose_range", self.lose_range)?;
        if self.lose_range < self.range {
            return Err("sight.lose_range must be at least sight.range".into());
        }
        check_angle("sight.half_angle", self.half_angle)?;
        check_angle("sight.peripheral_half_angle", self.peripheral_half_angle)?;
        if self.peripheral_half_angle < self.half_angle {
            return Err("sight.peripheral_half_angle must be at least sight.half_angle".into());
        }
        if !self.height_offset.is_finite() || self.height_offset.abs() > 1000.0 {
            return Err("sight.height_offset must be within +/-1000".into());
        }
        check_age(self.max_age)
    }
}

/// Hearing: noises within range, scaled by loudness and distance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HearingConfig {
    /// Farthest noise this listener can hear, whatever the noise's own range.
    pub range: f32,
    /// Noises whose strength at the listener is below this are not heard.
    pub threshold: f32,
    pub max_age: f32,
    pub affiliations: AffiliationFilter,
}

impl Default for HearingConfig {
    fn default() -> Self {
        Self {
            range: 30.0,
            threshold: 0.05,
            max_age: 5.0,
            affiliations: AffiliationFilter::HOSTILE,
        }
    }
}

impl HearingConfig {
    /// # Errors
    /// A bound that is violated, with a message naming the field.
    pub fn validate(&self) -> Result<(), String> {
        check_range("hearing.range", self.range)?;
        if !self.threshold.is_finite() || !(0.0..=1.0).contains(&self.threshold) {
            return Err("hearing.threshold must be in [0, 1]".into());
        }
        check_age(self.max_age)
    }
}

/// Damage: the victim senses its attacker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DamageConfig {
    pub max_age: f32,
    pub affiliations: AffiliationFilter,
}

impl Default for DamageConfig {
    fn default() -> Self {
        Self {
            max_age: 10.0,
            affiliations: AffiliationFilter::ALL,
        }
    }
}

impl DamageConfig {
    /// # Errors
    /// A bound that is violated, with a message naming the field.
    pub fn validate(&self) -> Result<(), String> {
        check_age(self.max_age)
    }
}

/// The senses of one perceiver.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SensorConfig {
    pub team: u32,
    pub sight: Option<SightConfig>,
    pub hearing: Option<HearingConfig>,
    pub damage: Option<DamageConfig>,
}

impl SensorConfig {
    pub(crate) fn max_age(&self, sense: Sense) -> Option<f32> {
        match sense {
            Sense::Sight => self.sight.as_ref().map(|s| s.max_age),
            Sense::Hearing => self.hearing.as_ref().map(|s| s.max_age),
            Sense::Damage => self.damage.as_ref().map(|s| s.max_age),
        }
    }
}
