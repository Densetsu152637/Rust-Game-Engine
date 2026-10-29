//! Voice chat carried on `Lane::VOICE` through the session. Capture, playback and real codecs
//! stay native (the audio domain); frames are 16-bit mono PCM handed to a [`VoiceCodec`].
use super::{NetworkSession, delta::Side, error, settings::VoiceSettings, voice_error};
use crate::Result;
use bevy_ecs::prelude::*;
use engine_net_voice::{
    RawPcmCodec, RouterStats, VOICE_DELIVERY, VOICE_LANE, VoiceCodec, VoiceOutput, VoiceReceiver,
    VoiceRouter, VoiceSender,
};
use engine_network::{MessageEndpoint, PeerId, SERVER_PEER};
use serde::Serialize;
use std::{collections::BTreeMap, sync::Arc, sync::Mutex};

type Factory = Arc<dyn Fn() -> engine_net_voice::Result<Box<dyn VoiceCodec>> + Send + Sync>;

fn factory(settings: &VoiceSettings) -> Result<Factory> {
    if let Some(codec) = &settings.codec {
        return Ok(codec.0.clone());
    }
    let codec = RawPcmCodec::new(settings.sample_rate, settings.frame_ms).map_err(voice_error)?;
    Ok(Arc::new(move || Ok(Box::new(codec) as Box<dyn VoiceCodec>)))
}

#[cfg(test)]
mod codec_factory_tests {
    use super::super::settings::VoiceCodecFactory;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn decoder_initialization_failure_propagates_from_client_creation() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let settings = VoiceSettings {
            codec: Some(VoiceCodecFactory(Arc::new(move || {
                if observed.fetch_add(1, Ordering::Relaxed) == 0 {
                    Ok(Box::new(RawPcmCodec::new(16_000, 20).unwrap()) as Box<dyn VoiceCodec>)
                } else {
                    Err(engine_net_voice::VoiceError::Codec("decoder init failed"))
                }
            }))),
            ..VoiceSettings::default()
        };
        assert!(ClientVoice::new(&settings).is_err());
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}

/// Nonsecret voice counters in the status report.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct VoiceStatus {
    /// Server: frames received from speakers.
    pub frames_in: u64,
    /// Server: downlink copies sent to listeners.
    pub frames_out: u64,
    /// Server: frames refused by the router (malformed, rate limited, moderation).
    pub dropped: u64,
    /// Server: copies removed by a speaker's `proximity_distance` narrowing.
    pub filtered: u64,
    pub router: Option<RouterStats>,
    /// Client: frames sent / received / played out and how many playout frames were concealed.
    pub frames_sent: u64,
    pub frames_received: u64,
    pub frames_pulled: u64,
    pub frames_concealed: u64,
    pub decode_errors: u64,
    pub speakers: usize,
}

pub(crate) struct ServerVoice {
    pub(crate) router: VoiceRouter,
    pub(crate) positions: BTreeMap<PeerId, [f32; 3]>,
    pub(crate) narrow: BTreeMap<PeerId, f32>,
    pub(crate) channels: BTreeMap<PeerId, u8>,
    pub(crate) status: VoiceStatus,
}
impl ServerVoice {
    pub(crate) fn new(settings: &VoiceSettings) -> Result<Self> {
        let router =
            VoiceRouter::new(settings.limits.clone(), settings.routing()).map_err(voice_error)?;
        Ok(Self {
            router,
            positions: BTreeMap::new(),
            narrow: BTreeMap::new(),
            channels: BTreeMap::new(),
            status: VoiceStatus::default(),
        })
    }
    pub(crate) fn add_peer(&mut self, peer: PeerId) {
        self.router.add_peer(peer);
    }
    pub(crate) fn remove_peer(&mut self, peer: PeerId) {
        self.router.remove_peer(peer);
        self.positions.remove(&peer);
        self.narrow.remove(&peer);
        self.channels.remove(&peer);
    }
    /// Routes one uplink frame; returns `(listener, bytes)` copies after narrowing.
    pub(crate) fn route(
        &mut self,
        speaker: PeerId,
        payload: &[u8],
        now_ms: f64,
    ) -> Vec<(PeerId, Vec<u8>)> {
        self.status.frames_in += 1;
        let Ok(copies) = self.router.route(speaker, payload, now_ms) else {
            self.status.dropped += 1;
            return Vec::new();
        };
        let narrow = self.narrow.get(&speaker).copied().filter(|d| *d > 0.0);
        let offered = copies.len();
        let out: Vec<_> = copies
            .into_iter()
            .filter(|(listener, _)| {
                let Some(limit) = narrow else { return true };
                match (self.positions.get(&speaker), self.positions.get(listener)) {
                    (Some(a), Some(b)) => {
                        let d2: f32 = a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum();
                        d2 <= limit * limit
                    }
                    _ => false,
                }
            })
            .collect();
        self.status.filtered += (offered - out.len()) as u64;
        self.status.frames_out += out.len() as u64;
        out
    }
    pub(crate) fn status(&self) -> VoiceStatus {
        VoiceStatus {
            router: Some(self.router.stats()),
            ..self.status.clone()
        }
    }
}

pub(crate) struct ClientVoice {
    sender: Mutex<VoiceSender>,
    receiver: Mutex<VoiceReceiver>,
    pub(crate) status: VoiceStatus,
}
impl ClientVoice {
    pub(crate) fn new(settings: &VoiceSettings) -> Result<Self> {
        let make = factory(settings)?;
        let sender = VoiceSender::new(make().map_err(voice_error)?, settings.limits.clone(), 0)
            .map_err(voice_error)?;
        let receiver = VoiceReceiver::new(
            settings.limits.clone(),
            settings.jitter,
            Box::new(move || make()),
        )
        .map_err(voice_error)?;
        Ok(Self {
            sender: Mutex::new(sender),
            receiver: Mutex::new(receiver),
            status: VoiceStatus::default(),
        })
    }
    pub(crate) fn receive(&mut self, payload: &[u8], now_ms: f64) {
        let receiver = self.receiver.get_mut().unwrap_or_else(|e| e.into_inner());
        if receiver.receive(payload, now_ms).is_ok() {
            self.status.frames_received += 1;
        }
    }
    pub(crate) fn status(&mut self) -> VoiceStatus {
        let receiver = self.receiver.get_mut().unwrap_or_else(|e| e.into_inner());
        VoiceStatus {
            decode_errors: receiver.decode_errors(),
            speakers: receiver.speakers().count(),
            ..self.status.clone()
        }
    }
}

fn session_mut(world: &mut World) -> Result<Mut<'_, NetworkSession>> {
    world
        .get_resource_mut::<NetworkSession>()
        .ok_or_else(|| error("No network session"))
}

/// Client: encodes and sends one PCM frame (`frame_samples` mono samples) on the voice lane.
/// Set `end_of_talkspurt` on the last frame before going silent.
pub fn send_voice_frame(world: &mut World, pcm: &[i16], end_of_talkspurt: bool) -> Result<()> {
    let mut session = session_mut(world)?;
    let NetworkSession {
        endpoint, delta, ..
    } = &mut *session;
    let (Some(super::Endpoint::LaneClient(lane)), Some(delta)) =
        (endpoint.as_mut(), delta.as_mut())
    else {
        return Err(error("Voice requires a delta-mode client session"));
    };
    let Side::Client(client) = &mut delta.side else {
        return Err(error("Voice requires a delta-mode client session"));
    };
    let voice = client
        .voice
        .as_mut()
        .ok_or_else(|| error("Voice is not enabled in the network settings"))?;
    let bytes = voice
        .sender
        .get_mut()
        .unwrap_or_else(|e| e.into_inner())
        .encode_frame(pcm, None, end_of_talkspurt)
        .map_err(voice_error)?;
    lane.get_mut()
        .unwrap_or_else(|e| e.into_inner())
        .send(SERVER_PEER, VOICE_LANE, VOICE_DELIVERY, &bytes)
        .map_err(super::transport)?;
    voice.status.frames_sent += 1;
    Ok(())
}

/// Client: the voice channel carried by subsequent frames.
pub fn set_voice_channel(world: &mut World, channel: u8) -> Result<()> {
    let mut session = session_mut(world)?;
    let Some(delta) = session.delta.as_mut() else {
        return Err(error("Voice requires a delta-mode client session"));
    };
    let Side::Client(client) = &mut delta.side else {
        return Err(error("Voice requires a delta-mode client session"));
    };
    let voice = client
        .voice
        .as_mut()
        .ok_or_else(|| error("Voice is not enabled in the network settings"))?;
    voice
        .sender
        .get_mut()
        .unwrap_or_else(|e| e.into_inner())
        .set_channel(channel);
    Ok(())
}

/// Client: at most one decoded (or concealed) frame per active speaker. Call once per frame
/// interval; time is the session's simulation clock.
pub fn pull_voice(world: &mut World) -> Vec<VoiceOutput> {
    let Some(mut session) = world.get_resource_mut::<NetworkSession>() else {
        return Vec::new();
    };
    let Some(delta) = session.delta.as_mut() else {
        return Vec::new();
    };
    let now_ms = delta.now_ms;
    let Side::Client(client) = &mut delta.side else {
        return Vec::new();
    };
    let Some(voice) = client.voice.as_mut() else {
        return Vec::new();
    };
    let out = voice
        .receiver
        .get_mut()
        .unwrap_or_else(|e| e.into_inner())
        .pull(now_ms);
    voice.status.frames_pulled += out.len() as u64;
    voice.status.frames_concealed += out.iter().filter(|o| o.concealed).count() as u64;
    out
}

fn server_voice<R>(
    world: &mut World,
    run: impl FnOnce(&mut ServerVoice) -> engine_net_voice::Result<R>,
) -> Result<R> {
    let mut session = session_mut(world)?;
    let Some(delta) = session.delta.as_mut() else {
        return Err(error(
            "Voice moderation requires a delta-mode server session",
        ));
    };
    let Side::Server(server) = &mut delta.side else {
        return Err(error(
            "Voice moderation requires a delta-mode server session",
        ));
    };
    let voice = server
        .voice
        .as_mut()
        .ok_or_else(|| error("Voice is not enabled in the network settings"))?;
    run(voice).map_err(voice_error)
}

/// Server: `listener` stops (or resumes) hearing `speaker`.
pub fn set_voice_muted(
    world: &mut World,
    listener: PeerId,
    speaker: PeerId,
    muted: bool,
) -> Result<()> {
    server_voice(world, |v| v.router.set_muted(listener, speaker, muted))
}

/// Server moderation: a disallowed speaker's frames are dropped.
pub fn set_voice_speaking_allowed(world: &mut World, peer: PeerId, allowed: bool) -> Result<()> {
    server_voice(world, |v| v.router.set_speaking_allowed(peer, allowed))
}
