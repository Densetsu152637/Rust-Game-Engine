//! Sans-IO control agents sharing one datagram socket.
use crate::wire;
use engine_net_quic::DatagramSocket;
use std::{io, net::SocketAddr, time::Instant};

/// One datagram to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transmit {
    pub destination: SocketAddr,
    pub payload: Vec<u8>,
}

/// A sans-IO participant of the control protocol ([`crate::ReflexiveClient`],
/// [`crate::PunchSession`]). Several agents can share one socket: each received control
/// datagram is offered to the agents in turn until one consumes it.
pub trait ControlAgent {
    /// Consumes `datagram` and returns true when it belongs to this agent. Malformed,
    /// unauthenticated or unexpected datagrams are counted and ignored, never errors.
    fn handle(&mut self, now: Instant, from: SocketAddr, datagram: &[u8]) -> bool;
    /// The next datagram that is due, if any. Call until it returns `None`.
    fn poll_transmit(&mut self, now: Instant) -> Option<Transmit>;
}

/// Largest control datagram the agents read; longer datagrams are not control traffic.
const AGENT_BUFFER_BYTES: usize = 2048;

/// Reads up to `max_datagrams` datagrams from `socket`, offers control datagrams to `agents`
/// (others are dropped), then sends every due datagram. Send failures are packet loss.
/// Use a [`crate::NatChannel`] as `socket` when QUIC shares the underlying UDP socket.
/// Returns the number of datagrams read that no agent consumed.
pub fn drive_agents(
    socket: &mut dyn DatagramSocket,
    now: Instant,
    agents: &mut [&mut dyn ControlAgent],
    max_datagrams: usize,
) -> io::Result<usize> {
    let mut buffer = [0_u8; AGENT_BUFFER_BYTES];
    let mut unclaimed = 0;
    for _ in 0..max_datagrams {
        let (len, from) = match socket.recv_from(&mut buffer) {
            Ok(received) => received,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) if is_transient(&e) => continue,
            Err(e) => return Err(e),
        };
        let datagram = &buffer[..len.min(AGENT_BUFFER_BYTES)];
        let claimed = wire::is_control_datagram(datagram)
            && agents
                .iter_mut()
                .any(|agent| agent.handle(now, from, datagram));
        unclaimed += usize::from(!claimed);
    }
    for agent in agents.iter_mut() {
        while let Some(transmit) = agent.poll_transmit(now) {
            let _ = socket.send_to(&transmit.payload, transmit.destination);
        }
    }
    Ok(unclaimed)
}

/// Errors the QUIC socket contract treats as transient (Windows reports ICMP port
/// unreachable as `ConnectionReset`).
pub(crate) fn is_transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::Interrupted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionRefused
    )
}
