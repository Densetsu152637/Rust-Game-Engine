//! Deterministic test support: a seeded RNG, a lossy/reordering/duplicating link with
//! independent behaviour per lane, and a small server-plus-clients simulation driver.
//!
//! Everything here is seeded and tick-driven, so a failing seed reproduces exactly.
use crate::{
    OutPacket, ReplicationClient, ReplicationError, ReplicationLimits, ReplicationRegistry,
    ReplicationServer, ServerConfig,
};
use engine_network::{Delivery, Lane, PeerId};
use std::sync::Arc;

/// xorshift64* generator.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    /// Uniform in `0..n` (`n == 0` returns 0).
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }
    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
}

/// Impairments applied to unreliable payloads; reliable payloads are delayed but never lost.
#[derive(Clone, Debug)]
pub struct LinkConfig {
    pub loss: f32,
    pub duplicate: f32,
    /// Fixed delay in ticks.
    pub base_delay: u32,
    /// Extra random delay `0..=jitter` ticks; this is what reorders packets.
    pub jitter: u32,
}

impl LinkConfig {
    pub fn perfect() -> Self {
        Self {
            loss: 0.0,
            duplicate: 0.0,
            base_delay: 0,
            jitter: 0,
        }
    }
    /// 20% loss, 10% duplication, up to 4 ticks of reordering jitter.
    pub fn hostile() -> Self {
        Self {
            loss: 0.2,
            duplicate: 0.1,
            base_delay: 1,
            jitter: 4,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Delivered {
    pub lane: Lane,
    pub delivery: Delivery,
    pub bytes: Vec<u8>,
}

struct InFlight {
    due: u32,
    order: u64,
    item: Delivered,
}

/// One direction of a link. Each lane draws from its own RNG stream and keeps its own
/// reliable-ordered queue, so lanes never influence each other.
pub struct LossyLink {
    cfg: LinkConfig,
    rngs: Vec<Rng>,
    last_ordered_due: [u32; 16],
    queue: Vec<InFlight>,
    order: u64,
    pub sent: u64,
    pub lost: u64,
    pub duplicated: u64,
    pub delivered: u64,
}

impl LossyLink {
    pub fn new(cfg: LinkConfig, seed: u64) -> Self {
        Self {
            cfg,
            rngs: (0..16)
                .map(|i| Rng::new(seed ^ ((i as u64 + 1) * 0x1234_5678_9abc)))
                .collect(),
            last_ordered_due: [0; 16],
            queue: Vec::new(),
            order: 0,
            sent: 0,
            lost: 0,
            duplicated: 0,
            delivered: 0,
        }
    }

    pub fn send(&mut self, now: u32, lane: Lane, delivery: Delivery, bytes: Vec<u8>) {
        self.sent += 1;
        let l = usize::from(lane.0.min(15));
        let jitter = self.rngs[l].below(u64::from(self.cfg.jitter) + 1) as u32;
        let mut due = now + self.cfg.base_delay + jitter;
        let mut copies = 1;
        if delivery.is_reliable() {
            if delivery == Delivery::ReliableOrdered {
                due = due.max(self.last_ordered_due[l]);
                self.last_ordered_due[l] = due;
            }
        } else if self.rngs[l].chance(self.cfg.loss) {
            self.lost += 1;
            return;
        } else if self.rngs[l].chance(self.cfg.duplicate) {
            copies = 2;
            self.duplicated += 1;
        }
        for copy in 0..copies {
            let extra = if copy == 0 {
                0
            } else {
                self.rngs[l].below(u64::from(self.cfg.jitter) + 2) as u32
            };
            self.order += 1;
            self.queue.push(InFlight {
                due: due + extra,
                order: self.order,
                item: Delivered {
                    lane,
                    delivery,
                    bytes: bytes.clone(),
                },
            });
        }
    }

    /// Everything due at or before `now`, in due order.
    pub fn deliver(&mut self, now: u32) -> Vec<Delivered> {
        let (mut due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.queue)
            .into_iter()
            .partition(|f| f.due <= now);
        self.queue = rest;
        due.sort_by_key(|f| (f.due, f.order));
        self.delivered += due.len() as u64;
        due.into_iter().map(|f| f.item).collect()
    }

    pub fn in_flight(&self) -> usize {
        self.queue.len()
    }
}

pub struct SimClient {
    pub peer: PeerId,
    pub client: ReplicationClient,
    pub down: LossyLink,
    pub up: LossyLink,
    /// Packets the client rejected (should stay 0 unless a test injects garbage).
    pub rejected: u64,
}

/// Bytes sent to one client during one `Simulation::step`.
#[derive(Clone, Debug, Default)]
pub struct StepStats {
    pub bytes: Vec<usize>,
    pub packets: Vec<usize>,
}

/// A server and N clients connected by lossy links.
pub struct Simulation {
    pub server: ReplicationServer,
    pub clients: Vec<SimClient>,
    pub tick: u32,
}

impl Simulation {
    /// Peers are numbered `1..=clients`.
    pub fn new(
        registry: Arc<ReplicationRegistry>,
        limits: ReplicationLimits,
        cfg: ServerConfig,
        link: &LinkConfig,
        seed: u64,
        clients: usize,
    ) -> Result<Self, ReplicationError> {
        let mut server = ReplicationServer::new(registry.clone(), limits.clone(), cfg)?;
        let mut sims = Vec::new();
        for i in 0..clients {
            let peer = i as PeerId + 1;
            server.add_peer(peer)?;
            sims.push(SimClient {
                peer,
                client: ReplicationClient::new(registry.clone(), limits.clone())?,
                down: LossyLink::new(link.clone(), seed.wrapping_add(i as u64 * 7919)),
                up: LossyLink::new(link.clone(), seed.wrapping_add(i as u64 * 104_729 + 1)),
                rejected: 0,
            });
        }
        Ok(Self {
            server,
            clients: sims,
            tick: 0,
        })
    }

    /// Advances one tick: build and send per client, deliver, acknowledge.
    pub fn step(&mut self, budget_bytes: usize, max_payload: usize) -> StepStats {
        self.tick += 1;
        let tick = self.tick;
        let mut stats = StepStats::default();
        for c in &mut self.clients {
            let packets = self
                .server
                .build(c.peer, tick, budget_bytes, max_payload)
                .expect("build");
            stats.packets.push(packets.len());
            stats
                .bytes
                .push(packets.iter().map(|p| p.bytes.len()).sum());
            for p in packets {
                c.down.send(tick, p.lane, p.delivery, p.bytes);
            }
            for d in c.down.deliver(tick) {
                if c.client.receive(&d.bytes).is_err() {
                    c.rejected += 1;
                }
            }
            if let Some(ack) = c.client.build_ack() {
                c.up.send(tick, ack.lane, ack.delivery, ack.bytes);
            }
            for d in c.up.deliver(tick) {
                let _ = self.server.receive(c.peer, &d.bytes);
            }
        }
        stats
    }

    /// Queues a client-originated packet (for example an RPC) on the client's uplink.
    pub fn client_send(&mut self, client: usize, packet: OutPacket) {
        let tick = self.tick;
        let c = &mut self.clients[client];
        c.up.send(tick, packet.lane, packet.delivery, packet.bytes);
    }

    /// Queues a server-originated packet (for example an RPC) on the client's downlink.
    pub fn server_send(&mut self, client: usize, packet: OutPacket) {
        let tick = self.tick;
        let c = &mut self.clients[client];
        c.down
            .send(tick, packet.lane, packet.delivery, packet.bytes);
    }
}
