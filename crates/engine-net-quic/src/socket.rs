//! The datagram socket contract under a QUIC endpoint.
use std::{
    io,
    net::{SocketAddr, UdpSocket},
};

/// Nonblocking datagram transport under [`crate::QuicServer`] and [`crate::QuicClient`].
///
/// This is a cross-crate contract: NAT traversal and relay layers implement it to hand
/// endpoints hole-punched or relay-wrapped paths. Implementations must never block:
///
/// - `send_to` transmits one whole datagram or fails. `WouldBlock` (or any error) means the
///   datagram is treated as lost; QUIC loss recovery retransmits what matters.
/// - `recv_from` returns one queued datagram and its source, or `WouldBlock` when none is
///   queued. `Interrupted` and `ConnectionReset` (reported by Windows after ICMP port
///   unreachable) are treated as transient; any other error is returned from `poll`.
///   A datagram longer than `buffer` may be truncated; QUIC then discards it.
/// - `local_addr` is diagnostic only; the endpoint never relies on it for routing.
///
/// Source addresses are untrusted input: QUIC authenticates packets, not addresses, and the
/// server validates new addresses with Retry by default.
pub trait DatagramSocket: Send + Sync {
    fn send_to(&mut self, datagram: &[u8], destination: SocketAddr) -> io::Result<usize>;
    fn recv_from(&mut self, buffer: &mut [u8]) -> io::Result<(usize, SocketAddr)>;
    fn local_addr(&self) -> io::Result<SocketAddr>;
}

/// The std UDP socket. It must be in nonblocking mode; the endpoints' `bind`/`connect`
/// constructors set this, while `with_socket` callers configure it themselves.
impl DatagramSocket for UdpSocket {
    fn send_to(&mut self, datagram: &[u8], destination: SocketAddr) -> io::Result<usize> {
        UdpSocket::send_to(self, datagram, destination)
    }
    fn recv_from(&mut self, buffer: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        UdpSocket::recv_from(self, buffer)
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        UdpSocket::local_addr(self)
    }
}

pub(crate) fn bind_nonblocking(addr: SocketAddr) -> io::Result<UdpSocket> {
    let socket = UdpSocket::bind(addr)?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}

/// Sends one datagram; every send failure is packet loss for QUIC, never a `poll` error.
pub(crate) fn send_lossy(socket: &mut dyn DatagramSocket, datagram: &[u8], to: SocketAddr) {
    let _ = socket.send_to(datagram, to);
}

pub(crate) enum Received {
    Datagram(usize, SocketAddr),
    /// A transient error consumed this receive attempt; keep reading.
    Transient,
    /// Nothing is queued.
    Empty,
}
/// Receives one datagram; `Err` only for a broken socket.
pub(crate) fn recv(socket: &mut dyn DatagramSocket, buffer: &mut [u8]) -> io::Result<Received> {
    match socket.recv_from(buffer) {
        Ok((len, from)) => Ok(Received::Datagram(len, from)),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(Received::Empty),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::Interrupted
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(Received::Transient)
        }
        Err(e) => Err(e),
    }
}
