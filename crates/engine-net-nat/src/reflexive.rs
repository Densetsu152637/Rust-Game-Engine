//! Server-reflexive address discovery (a minimal STUN-like binding exchange).
use crate::{
    ControlAgent, Error, Result, Transmit,
    wire::{self, TxId},
};
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

/// Retransmission policy of a [`ReflexiveClient`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflexiveConfig {
    /// Delay between retransmissions of the same transaction.
    pub retransmit_interval: Duration,
    /// Requests sent before the transaction times out.
    pub max_attempts: u32,
}
impl Default for ReflexiveConfig {
    fn default() -> Self {
        Self {
            retransmit_interval: Duration::from_millis(200),
            max_attempts: 5,
        }
    }
}
impl ReflexiveConfig {
    pub fn validate(&self) -> Result<()> {
        if self.retransmit_interval < Duration::from_millis(10)
            || self.retransmit_interval > Duration::from_secs(10)
            || self.max_attempts == 0
            || self.max_attempts > 32
        {
            return Err(Error::Invalid("invalid reflexive discovery config"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReflexiveOutcome {
    Pending,
    /// The address the server observed as this socket's source (the NAT mapping).
    Discovered(SocketAddr),
    TimedOut,
}

/// One binding transaction against one discovery server. A fresh random 96-bit transaction
/// ID is used for every client; only a response carrying that ID *and* arriving from the
/// server address is accepted. Use one client per server to compare mappings (two servers
/// reporting different ports reveal an address-dependent mapping, i.e. a symmetric NAT).
pub struct ReflexiveClient {
    server: SocketAddr,
    txid: TxId,
    config: ReflexiveConfig,
    attempts: u32,
    next_send: Instant,
    outcome: ReflexiveOutcome,
    rejected: u64,
}
impl ReflexiveClient {
    pub fn new(now: Instant, server: SocketAddr, config: ReflexiveConfig) -> Result<Self> {
        config.validate()?;
        if !wire::valid_endpoint(&server) {
            return Err(Error::Invalid("invalid discovery server address"));
        }
        Ok(Self {
            server,
            txid: wire::random_bytes()?,
            config,
            attempts: 0,
            next_send: now,
            outcome: ReflexiveOutcome::Pending,
            rejected: 0,
        })
    }
    pub fn server(&self) -> SocketAddr {
        self.server
    }
    pub fn outcome(&self) -> ReflexiveOutcome {
        self.outcome
    }
    /// Responses carrying this transaction ID that were ignored (wrong source address or a
    /// duplicate after completion).
    pub fn rejected(&self) -> u64 {
        self.rejected
    }
}
impl ControlAgent for ReflexiveClient {
    fn handle(&mut self, _now: Instant, from: SocketAddr, datagram: &[u8]) -> bool {
        let Some((txid, observed)) = wire::decode_binding_response(datagram) else {
            return false;
        };
        if !wire::ct_eq(&txid, &self.txid) {
            return false;
        }
        if from == self.server && self.outcome == ReflexiveOutcome::Pending {
            self.outcome = ReflexiveOutcome::Discovered(observed);
        } else {
            self.rejected += 1;
        }
        true
    }
    fn poll_transmit(&mut self, now: Instant) -> Option<Transmit> {
        if self.outcome != ReflexiveOutcome::Pending || now < self.next_send {
            return None;
        }
        if self.attempts >= self.config.max_attempts {
            self.outcome = ReflexiveOutcome::TimedOut;
            return None;
        }
        self.attempts += 1;
        self.next_send = now + self.config.retransmit_interval;
        let mut payload = Vec::with_capacity(wire::BINDING_REQUEST_LEN);
        wire::encode_binding_request(&mut payload, &self.txid);
        Some(Transmit {
            destination: self.server,
            payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_outstanding_transaction_from_the_server_completes() {
        let t0 = Instant::now();
        let server: SocketAddr = "127.0.0.1:3478".parse().unwrap();
        let mut client = ReflexiveClient::new(t0, server, ReflexiveConfig::default()).unwrap();
        let request = client.poll_transmit(t0).unwrap();
        assert_eq!(request.destination, server);
        assert_eq!(request.payload.len(), wire::BINDING_REQUEST_LEN);
        let txid = wire::decode_binding_request(&request.payload).unwrap();
        let observed: SocketAddr = "198.51.100.7:40000".parse().unwrap();
        let mut response = Vec::new();
        wire::encode_binding_response(&mut response, &[9; 12], observed);
        assert!(!client.handle(t0, server, &response), "foreign transaction");
        wire::encode_binding_response(&mut response, &txid, observed);
        let spoofer: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        assert!(client.handle(t0, spoofer, &response));
        assert_eq!(client.outcome(), ReflexiveOutcome::Pending, "wrong source");
        assert!(client.handle(t0, server, &response));
        assert_eq!(client.outcome(), ReflexiveOutcome::Discovered(observed));
        assert_eq!(client.rejected(), 1);
        assert!(client.poll_transmit(t0 + Duration::from_secs(1)).is_none());
    }

    #[test]
    fn retransmits_then_times_out() {
        let t0 = Instant::now();
        let config = ReflexiveConfig {
            retransmit_interval: Duration::from_millis(100),
            max_attempts: 3,
        };
        let server: SocketAddr = "127.0.0.1:3478".parse().unwrap();
        let mut client = ReflexiveClient::new(t0, server, config).unwrap();
        let mut sent = 0;
        for step in 0..10 {
            let now = t0 + Duration::from_millis(100 * step);
            sent += usize::from(client.poll_transmit(now).is_some());
            assert!(
                client.poll_transmit(now).is_none(),
                "one request per interval"
            );
        }
        assert_eq!(sent, 3);
        assert_eq!(client.outcome(), ReflexiveOutcome::TimedOut);
    }
}
