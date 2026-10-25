//! Transport-neutral endpoint wrapper used by delta mode.
use engine_network::{
    ConditionedEndpoint, Delivery, Lane, LinkEvent, LinkStats, MessageEndpoint, NetworkConditions,
    PeerId, PollBudget, Result, TransportKind, TransportSecurity,
};

/// A caller-supplied endpoint (TCP, QUIC, ...). `Box<dyn MessageEndpoint>` cannot implement the
/// trait directly (orphan rule), so this newtype delegates.
pub(crate) struct BoxedEndpoint(pub Box<dyn MessageEndpoint + Send>);

impl MessageEndpoint for BoxedEndpoint {
    fn transport(&self) -> TransportKind {
        self.0.transport()
    }
    fn security(&self) -> TransportSecurity {
        self.0.security()
    }
    fn connected_peers(&self) -> Vec<PeerId> {
        self.0.connected_peers()
    }
    fn max_payload(&self, peer: PeerId, delivery: Delivery) -> Option<usize> {
        self.0.max_payload(peer, delivery)
    }
    fn send(&mut self, peer: PeerId, lane: Lane, delivery: Delivery, payload: &[u8]) -> Result<()> {
        self.0.send(peer, lane, delivery, payload)
    }
    fn poll(&mut self, budget: PollBudget) -> Result<Vec<LinkEvent>> {
        self.0.poll(budget)
    }
    fn stats(&self, peer: PeerId) -> Option<LinkStats> {
        self.0.stats(peer)
    }
    fn disconnect(&mut self, peer: PeerId) -> bool {
        self.0.disconnect(peer)
    }
}

/// The attached endpoint, optionally wrapped in a network-condition simulator.
pub(crate) enum LaneEndpoint {
    Plain(BoxedEndpoint),
    Conditioned(Box<ConditionedEndpoint<BoxedEndpoint>>),
}

impl LaneEndpoint {
    pub(crate) fn new(
        inner: Box<dyn MessageEndpoint + Send>,
        conditions: Option<NetworkConditions>,
    ) -> Result<Self> {
        let inner = BoxedEndpoint(inner);
        Ok(match conditions {
            Some(c) => Self::Conditioned(Box::new(ConditionedEndpoint::new(inner, c)?)),
            None => Self::Plain(inner),
        })
    }
    /// Replaces the simulation. Events still held by the previous simulator are discarded.
    pub(crate) fn with_conditions(self, conditions: Option<NetworkConditions>) -> Result<Self> {
        let inner = match self {
            Self::Plain(inner) => inner,
            Self::Conditioned(c) => c.into_inner(),
        };
        Ok(match conditions {
            Some(c) => Self::Conditioned(Box::new(ConditionedEndpoint::new(inner, c)?)),
            None => Self::Plain(inner),
        })
    }
}

macro_rules! delegate {
    ($self:ident, $inner:ident => $call:expr) => {
        match $self {
            LaneEndpoint::Plain($inner) => $call,
            LaneEndpoint::Conditioned($inner) => $call,
        }
    };
}

impl MessageEndpoint for LaneEndpoint {
    fn transport(&self) -> TransportKind {
        delegate!(self, e => e.transport())
    }
    fn security(&self) -> TransportSecurity {
        delegate!(self, e => e.security())
    }
    fn connected_peers(&self) -> Vec<PeerId> {
        delegate!(self, e => e.connected_peers())
    }
    fn max_payload(&self, peer: PeerId, delivery: Delivery) -> Option<usize> {
        delegate!(self, e => e.max_payload(peer, delivery))
    }
    fn send(&mut self, peer: PeerId, lane: Lane, delivery: Delivery, payload: &[u8]) -> Result<()> {
        delegate!(self, e => e.send(peer, lane, delivery, payload))
    }
    fn poll(&mut self, budget: PollBudget) -> Result<Vec<LinkEvent>> {
        delegate!(self, e => e.poll(budget))
    }
    fn stats(&self, peer: PeerId) -> Option<LinkStats> {
        delegate!(self, e => e.stats(peer))
    }
    fn disconnect(&mut self, peer: PeerId) -> bool {
        delegate!(self, e => e.disconnect(peer))
    }
}
