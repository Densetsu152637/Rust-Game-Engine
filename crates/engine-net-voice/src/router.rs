use crate::{DownlinkPacket, Result, UplinkPacket, VoiceError, VoiceLimits};
use engine_network::PeerId;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Who hears a speaker.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VoiceRouting {
    /// Every other peer.
    Global,
    /// Peers that joined the packet's channel (the speaker must be a member too).
    Channel,
    /// Peers within `max_distance` of the speaker; both positions must be known.
    Proximity { max_distance: f32 },
    /// Channel membership AND proximity.
    ChannelProximity { max_distance: f32 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct RouterStats {
    pub received: u64,
    pub forwarded_copies: u64,
    pub rate_limited: u64,
    pub malformed: u64,
    pub muted_or_out_of_range: u64,
}

#[derive(Default)]
struct PeerVoice {
    channels: BTreeSet<u8>,
    position: Option<[f32; 3]>,
    /// Speakers this listener muted.
    muted: BTreeSet<PeerId>,
    window_start_ms: f64,
    window_frames: u32,
    speaking_allowed: bool,
}

/// Server-side voice forwarder. Speaker identity comes from the transport peer ID, never from
/// packet bytes, and positions come from the authoritative game unless explicitly trusted.
///
/// Downlink packets carry the speaker's position only under proximity routing (or after
/// [`VoiceRouter::set_send_positions`]): under `Global`/`Channel` routing every listener would
/// otherwise learn every speaker's server position, which is hidden information in most games.
pub struct VoiceRouter {
    limits: VoiceLimits,
    routing: VoiceRouting,
    trust_client_positions: bool,
    send_positions: bool,
    peers: BTreeMap<PeerId, PeerVoice>,
    stats: RouterStats,
}
impl VoiceRouter {
    pub fn new(limits: VoiceLimits, routing: VoiceRouting) -> Result<Self> {
        limits.validate()?;
        if let VoiceRouting::Proximity { max_distance }
        | VoiceRouting::ChannelProximity { max_distance } = routing
            && !(max_distance.is_finite() && max_distance > 0.0)
        {
            return Err(VoiceError::Invalid(
                "proximity distance must be finite and positive",
            ));
        }
        Ok(Self {
            limits,
            routing,
            trust_client_positions: false,
            send_positions: matches!(
                routing,
                VoiceRouting::Proximity { .. } | VoiceRouting::ChannelProximity { .. }
            ),
            peers: BTreeMap::new(),
            stats: RouterStats::default(),
        })
    }
    /// Accept speaker-claimed positions when the server has none (trusted/cooperative games only).
    pub fn set_trust_client_positions(&mut self, trust: bool) {
        self.trust_client_positions = trust;
    }
    /// Whether downlink packets carry the speaker's position for spatial voice. Defaults to
    /// true only for `Proximity`/`ChannelProximity` routing, where listeners are already in
    /// range. Enabling it under `Global`/`Channel` routing reveals every speaker's position to
    /// every listener that hears them.
    pub fn set_send_positions(&mut self, send: bool) {
        self.send_positions = send;
    }
    pub fn stats(&self) -> RouterStats {
        self.stats
    }
    pub fn add_peer(&mut self, peer: PeerId) {
        self.peers.entry(peer).or_insert_with(|| PeerVoice {
            speaking_allowed: true,
            ..PeerVoice::default()
        });
    }
    pub fn remove_peer(&mut self, peer: PeerId) {
        self.peers.remove(&peer);
        for other in self.peers.values_mut() {
            other.muted.remove(&peer);
        }
    }
    /// Authoritative position from the game simulation (used for proximity and output).
    pub fn set_position(&mut self, peer: PeerId, position: Option<[f32; 3]>) -> Result<()> {
        if position.is_some_and(|p| !crate::finite_position(p)) {
            return Err(VoiceError::Invalid("non-finite voice position"));
        }
        self.peer(peer)?.position = position;
        Ok(())
    }
    pub fn join_channel(&mut self, peer: PeerId, channel: u8) -> Result<()> {
        let max = self.limits.max_channels_per_peer;
        let state = self.peer(peer)?;
        if !state.channels.contains(&channel) && state.channels.len() >= max {
            return Err(VoiceError::Invalid("voice channel membership cap exceeded"));
        }
        state.channels.insert(channel);
        Ok(())
    }
    pub fn leave_channel(&mut self, peer: PeerId, channel: u8) -> Result<()> {
        self.peer(peer)?.channels.remove(&channel);
        Ok(())
    }
    /// Listener-side mute of one speaker.
    pub fn set_muted(&mut self, listener: PeerId, speaker: PeerId, muted: bool) -> Result<()> {
        let state = self.peer(listener)?;
        if muted {
            state.muted.insert(speaker);
        } else {
            state.muted.remove(&speaker);
        }
        Ok(())
    }
    /// Server-side moderation: a disallowed speaker's frames are dropped.
    pub fn set_speaking_allowed(&mut self, peer: PeerId, allowed: bool) -> Result<()> {
        self.peer(peer)?.speaking_allowed = allowed;
        Ok(())
    }
    fn peer(&mut self, peer: PeerId) -> Result<&mut PeerVoice> {
        self.peers
            .get_mut(&peer)
            .ok_or(VoiceError::Invalid("unknown voice peer"))
    }

    /// Routes one uplink payload received from `speaker` at `now_ms`. Returns the downlink bytes
    /// for each listener. Errors mean the packet was dropped; they are soft (do not disconnect
    /// for a single malformed voice packet unless the application chooses to).
    pub fn route(
        &mut self,
        speaker: PeerId,
        bytes: &[u8],
        now_ms: f64,
    ) -> Result<Vec<(PeerId, Vec<u8>)>> {
        self.stats.received += 1;
        let uplink = match UplinkPacket::decode(bytes, &self.limits) {
            Ok(packet) => packet,
            Err(error) => {
                self.stats.malformed += 1;
                return Err(error);
            }
        };
        let max_rate = self.limits.max_frames_per_second;
        let state = self.peer(speaker)?;
        let elapsed = now_ms - state.window_start_ms;
        if elapsed.is_nan() || !(0.0..1000.0).contains(&elapsed) {
            state.window_start_ms = now_ms;
            state.window_frames = 0;
        }
        state.window_frames += 1;
        if state.window_frames > max_rate || !state.speaking_allowed {
            self.stats.rate_limited += 1;
            return Err(VoiceError::RateLimited);
        }
        let speaker_channels = state.channels.clone();
        let speaker_position = state.position.or(if self.trust_client_positions {
            uplink.position
        } else {
            None
        });
        let downlink = DownlinkPacket {
            speaker,
            channel: uplink.channel,
            sequence: uplink.sequence,
            timestamp: uplink.timestamp,
            position: speaker_position.filter(|_| self.send_positions),
            end_of_talkspurt: uplink.end_of_talkspurt,
            payload: uplink.payload,
        }
        .encode(&self.limits)?;
        let channel_ok = |listener: &PeerVoice| {
            speaker_channels.contains(&uplink.channel)
                && listener.channels.contains(&uplink.channel)
        };
        let in_range = |listener: &PeerVoice, max: f32| match (speaker_position, listener.position)
        {
            (Some(a), Some(b)) => {
                let d2: f32 = a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum();
                d2 <= max * max
            }
            _ => false,
        };
        let mut out = Vec::new();
        for (&listener_id, listener) in &self.peers {
            if listener_id == speaker {
                continue;
            }
            let audible = !listener.muted.contains(&speaker)
                && match self.routing {
                    VoiceRouting::Global => true,
                    VoiceRouting::Channel => channel_ok(listener),
                    VoiceRouting::Proximity { max_distance } => in_range(listener, max_distance),
                    VoiceRouting::ChannelProximity { max_distance } => {
                        channel_ok(listener) && in_range(listener, max_distance)
                    }
                };
            if audible {
                out.push((listener_id, downlink.clone()));
            } else {
                self.stats.muted_or_out_of_range += 1;
            }
        }
        self.stats.forwarded_copies += out.len() as u64;
        Ok(out)
    }
}
