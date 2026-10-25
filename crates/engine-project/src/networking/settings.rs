//! Native session settings and the authored `engine.NetworkSession` component that fills them.
use super::{components::SESSION_COMPONENT, error};
use crate::{Result, Scene};
use engine_net_voice::{JitterConfig, VoiceCodec, VoiceLimits, VoiceRouting};
use engine_network::{NetworkConditions, PollBudget};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt, sync::Arc};

/// How authoritative state travels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    /// Legacy whole-snapshot replication over the TCP `Server`/`Client` event API.
    #[default]
    Snapshot,
    /// `engine-net-replication` deltas on `Lane::REPLICATION` and RPCs on `Lane::RPC` over any
    /// `MessageEndpoint`.
    Delta,
}

/// Storage type of one custom replicated state property.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateKind {
    Bool,
    I32,
    F32,
    /// UTF-8 text of at most 64 bytes.
    String,
}

/// One custom per-entity replicated property (`engine.state` component). Both ends must
/// configure the same list in the same order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateProperty {
    pub name: String,
    pub kind: StateKind,
}

/// Who hears a speaker (see `docs/networking-voice.md`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceRoutingKind {
    Global,
    Channel,
    #[default]
    Proximity,
    ChannelProximity,
}

/// Creates one decoder per speaker (and the encoder), so a real codec plugs in natively.
#[derive(Clone)]
pub struct VoiceCodecFactory(pub Arc<dyn Fn() -> Box<dyn VoiceCodec> + Send + Sync>);
impl fmt::Debug for VoiceCodecFactory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VoiceCodecFactory")
    }
}

#[derive(Clone, Debug)]
pub struct VoiceSettings {
    /// Raw 16-bit PCM parameters used when `codec` is `None`. 16 kHz and 20 ms (640 byte
    /// payloads) fit a QUIC datagram.
    pub sample_rate: u32,
    pub frame_ms: u32,
    pub routing: VoiceRoutingKind,
    /// Hearing distance for the proximity routings; per-speaker `engine.NetworkVoice`
    /// distances can only narrow it.
    pub max_distance: f32,
    pub limits: VoiceLimits,
    pub jitter: JitterConfig,
    pub codec: Option<VoiceCodecFactory>,
}
impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            sample_rate: 16_000,
            frame_ms: 20,
            routing: VoiceRoutingKind::Proximity,
            max_distance: 50.0,
            limits: VoiceLimits::default(),
            jitter: JitterConfig::default(),
            codec: None,
        }
    }
}
impl VoiceSettings {
    pub(crate) fn routing(&self) -> VoiceRouting {
        let max_distance = self.max_distance;
        match self.routing {
            VoiceRoutingKind::Global => VoiceRouting::Global,
            VoiceRoutingKind::Channel => VoiceRouting::Channel,
            VoiceRoutingKind::Proximity => VoiceRouting::Proximity { max_distance },
            VoiceRoutingKind::ChannelProximity => VoiceRouting::ChannelProximity { max_distance },
        }
    }
}

#[derive(Clone, Debug)]
pub struct NetworkSettings {
    pub poll_budget: PollBudget,
    /// Simulation tick duration; also the replication tick rate (rounded to whole Hz).
    pub tick_seconds: f64,
    /// Snapshot mode only: interpolation delay in simulation ticks. In delta mode it offsets
    /// `view_tick` for lag compensation.
    pub interpolation_delay_ticks: f64,
    /// Used by the TCP `attach_server`/`attach_client`; endpoint attachment is always `Delta`.
    pub mode: NetworkMode,
    /// Loss/latency simulation applied to received traffic (delta mode only).
    pub conditions: Option<NetworkConditions>,
    /// Per-peer replication bandwidth budget in bytes per second.
    pub bytes_per_second: u64,
    /// Default interest radius of a peer's view.
    pub view_radius: f32,
    /// Most replicated entities (also bounds client mirrors). Use the same value on both ends.
    pub max_entities: usize,
    /// Per-peer rate cap of client-to-server RPCs.
    pub rpc_calls_per_second: u32,
    /// When set, only these RPC names become `network.rpc` events; others count as rejected.
    pub rpc_allowlist: Option<BTreeSet<String>>,
    pub state_properties: Vec<StateProperty>,
    /// Emit `network.property_changed` for pose properties too (state properties always emit).
    pub property_events: bool,
    /// Client pose smoothing rate in 1/s; 0 snaps to the latest authoritative pose.
    pub remote_smoothing_hz: f32,
    /// Server lag compensation: oldest tick a client may claim to see.
    pub lag_rewind_ticks: u32,
    /// Ticks of hitbox history retained (at least `lag_rewind_ticks`).
    pub history_ticks: usize,
    pub voice: Option<VoiceSettings>,
    /// Peers listed in a status report (links and replication); extras set `*_truncated`.
    pub max_reported_peers: usize,
}
impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            poll_budget: PollBudget::default(),
            tick_seconds: 1.0 / 60.0,
            interpolation_delay_ticks: 1.0,
            mode: NetworkMode::Snapshot,
            conditions: None,
            bytes_per_second: 256 * 1024,
            view_radius: 100.0,
            max_entities: 4096,
            rpc_calls_per_second: 30,
            rpc_allowlist: None,
            state_properties: Vec::new(),
            property_events: false,
            remote_smoothing_hz: 0.0,
            lag_rewind_ticks: 12,
            history_ticks: 64,
            voice: None,
            max_reported_peers: 16,
        }
    }
}
impl NetworkSettings {
    pub(crate) fn validate(&self) -> Result<()> {
        if !self.tick_seconds.is_finite()
            || self.tick_seconds <= 0.0
            || !self.interpolation_delay_ticks.is_finite()
            || self.interpolation_delay_ticks < 0.0
        {
            return Err(error(
                "Require positive tick_seconds and nonnegative interpolation delay",
            ));
        }
        if let Some(conditions) = &self.conditions {
            conditions.validate().map_err(super::transport)?;
        }
        if self.bytes_per_second == 0
            || !self.view_radius.is_finite()
            || self.view_radius < 0.0
            || self.max_entities == 0
            || self.max_entities > 65_536
            || self.rpc_calls_per_second == 0
            || !self.remote_smoothing_hz.is_finite()
            || self.remote_smoothing_hz < 0.0
            || self.lag_rewind_ticks == 0
            || self.history_ticks < self.lag_rewind_ticks as usize
            || self.history_ticks > 4096
            || self.max_reported_peers == 0
            || self.state_properties.len() > 32
        {
            return Err(error("Invalid network replication settings"));
        }
        let mut names = BTreeSet::new();
        for property in &self.state_properties {
            if property.name.is_empty()
                || property.name.len() > 64
                || property.name.chars().any(char::is_control)
                || !names.insert(property.name.as_str())
            {
                return Err(error("State property names must be unique identifiers"));
            }
        }
        if let Some(voice) = &self.voice {
            voice.limits.validate().map_err(super::voice_error)?;
            voice.jitter.validate().map_err(super::voice_error)?;
            if !voice.max_distance.is_finite() || voice.max_distance <= 0.0 {
                return Err(error("Voice distance must be positive"));
            }
        }
        Ok(())
    }
    pub(crate) fn tick_rate_hz(&self) -> u32 {
        ((1.0 / self.tick_seconds).round() as u32).clamp(1, 1000)
    }

    /// Defaults taken from the scene's `engine.NetworkSession` component, if any. The mode,
    /// conditions, bandwidth, view radius and other fields written there (for example through
    /// the MCP `network_settings_set` tool) replace the built-in defaults; native code can
    /// then adjust the returned value before attaching.
    pub fn from_scene(scene: &Scene) -> Result<Self> {
        let mut settings = Self::default();
        let mut found = scene
            .entities
            .iter()
            .filter_map(|e| e.components.custom.get(SESSION_COMPONENT));
        if let Some(value) = found.next() {
            let config: NetworkSessionConfig = serde_json::from_value(value.data.clone())
                .map_err(|e| error(format!("engine.NetworkSession: {e}")))?;
            config.validate()?;
            config.apply(&mut settings);
        }
        if found.next().is_some() {
            return Err(error("At most one engine.NetworkSession per scene"));
        }
        settings.validate()?;
        Ok(settings)
    }
    /// [`Self::from_scene`] for the runtime's active scene.
    pub fn from_runtime(runtime: &crate::Runtime) -> Result<Self> {
        Self::from_scene(runtime.active_scene())
    }
}

/// Authored voice defaults inside `engine.NetworkSession`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VoiceConfig {
    pub enabled: bool,
    pub sample_rate: u32,
    pub frame_ms: u32,
    pub routing: VoiceRoutingKind,
    pub max_distance: f32,
}
impl Default for VoiceConfig {
    fn default() -> Self {
        let d = VoiceSettings::default();
        Self {
            enabled: false,
            sample_rate: d.sample_rate,
            frame_ms: d.frame_ms,
            routing: d.routing,
            max_distance: d.max_distance,
        }
    }
}

/// Authored link impairment: [`NetworkConditions`] without nullable fields.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConditionsConfig {
    pub loss: f64,
    pub latency_ms: u32,
    pub jitter_ms: u32,
    pub duplicate: f64,
    pub reorder: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bandwidth_bytes_per_second: Option<u64>,
    pub seed: u64,
}
impl ConditionsConfig {
    pub fn conditions(&self) -> NetworkConditions {
        NetworkConditions {
            loss: self.loss,
            latency_ms: self.latency_ms,
            jitter_ms: self.jitter_ms,
            duplicate: self.duplicate,
            reorder: self.reorder,
            bandwidth_bytes_per_second: self.bandwidth_bytes_per_second,
            seed: self.seed,
        }
    }
}
impl From<&NetworkConditions> for ConditionsConfig {
    fn from(c: &NetworkConditions) -> Self {
        Self {
            loss: c.loss,
            latency_ms: c.latency_ms,
            jitter_ms: c.jitter_ms,
            duplicate: c.duplicate,
            reorder: c.reorder,
            bandwidth_bytes_per_second: c.bandwidth_bytes_per_second,
            seed: c.seed,
        }
    }
}

/// Data of the singleton `engine.NetworkSession` component: the project-level network defaults
/// that headless and MCP-driven runs read without native code.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkSessionConfig {
    pub mode: NetworkMode,
    /// All-zero conditions mean "no simulation".
    pub conditions: ConditionsConfig,
    pub bytes_per_second: u64,
    pub view_radius: f32,
    pub max_entities: u32,
    pub rpc_calls_per_second: u32,
    pub property_events: bool,
    pub remote_smoothing_hz: f32,
    pub lag_rewind_ticks: u32,
    pub history_ticks: u32,
    pub voice: VoiceConfig,
}
impl Default for NetworkSessionConfig {
    fn default() -> Self {
        let d = NetworkSettings::default();
        Self {
            mode: NetworkMode::Snapshot,
            conditions: ConditionsConfig::default(),
            bytes_per_second: d.bytes_per_second,
            view_radius: d.view_radius,
            max_entities: d.max_entities as u32,
            rpc_calls_per_second: d.rpc_calls_per_second,
            property_events: d.property_events,
            remote_smoothing_hz: d.remote_smoothing_hz,
            lag_rewind_ticks: d.lag_rewind_ticks,
            history_ticks: d.history_ticks as u32,
            voice: VoiceConfig::default(),
        }
    }
}
impl NetworkSessionConfig {
    pub fn validate(&self) -> Result<()> {
        let bad = |what: &str| error(format!("engine.NetworkSession: invalid {what}"));
        self.conditions
            .conditions()
            .validate()
            .map_err(|_| bad("conditions"))?;
        if self.bytes_per_second == 0 || self.bytes_per_second > 1 << 32 {
            return Err(bad("bytes_per_second"));
        }
        if !self.view_radius.is_finite() || !(0.0..=1.0e6).contains(&self.view_radius) {
            return Err(bad("view_radius"));
        }
        if !(1..=65_536).contains(&self.max_entities)
            || !(1..=10_000).contains(&self.rpc_calls_per_second)
            || !self.remote_smoothing_hz.is_finite()
            || !(0.0..=1000.0).contains(&self.remote_smoothing_hz)
            || !(1..=4096).contains(&self.lag_rewind_ticks)
            || !(self.lag_rewind_ticks..=4096).contains(&self.history_ticks)
        {
            return Err(bad("replication fields"));
        }
        let voice = &self.voice;
        if !(8_000..=48_000).contains(&voice.sample_rate)
            || !(5..=60).contains(&voice.frame_ms)
            || !voice.max_distance.is_finite()
            || !(0.01..=1.0e6).contains(&voice.max_distance)
        {
            return Err(bad("voice"));
        }
        Ok(())
    }
    pub(crate) fn apply(&self, settings: &mut NetworkSettings) {
        settings.mode = self.mode;
        let conditions = self.conditions.conditions();
        settings.conditions = (conditions != NetworkConditions::default()).then_some(conditions);
        settings.bytes_per_second = self.bytes_per_second;
        settings.view_radius = self.view_radius;
        settings.max_entities = self.max_entities as usize;
        settings.rpc_calls_per_second = self.rpc_calls_per_second;
        settings.property_events = self.property_events;
        settings.remote_smoothing_hz = self.remote_smoothing_hz;
        settings.lag_rewind_ticks = self.lag_rewind_ticks;
        settings.history_ticks = self.history_ticks as usize;
        settings.voice = self.voice.enabled.then(|| VoiceSettings {
            sample_rate: self.voice.sample_rate,
            frame_ms: self.voice.frame_ms,
            routing: self.voice.routing,
            max_distance: self.voice.max_distance,
            ..VoiceSettings::default()
        });
    }
}
