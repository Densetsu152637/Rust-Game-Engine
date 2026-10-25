//! One UDP socket shared by a QUIC endpoint and the NAT/relay control layer.
use crate::{Error, Result, agent::is_transient, wire};
use engine_net_quic::DatagramSocket;
use serde::Serialize;
use std::{
    collections::VecDeque,
    io,
    net::SocketAddr,
    sync::{Arc, Mutex, MutexGuard},
};

/// Queue bounds of a [`DemuxSocket`] pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DemuxConfig {
    /// Control datagrams held for the [`NatChannel`] until it reads them.
    pub max_control_datagrams: usize,
    pub max_control_bytes: usize,
    /// QUIC datagrams read by the [`NatChannel`] and held for the QUIC side.
    pub max_quic_datagrams: usize,
    pub max_quic_bytes: usize,
    /// Socket reads one `recv_from` call may perform while skipping the other side's traffic.
    pub max_reads_per_call: usize,
}
impl Default for DemuxConfig {
    fn default() -> Self {
        Self {
            max_control_datagrams: 256,
            max_control_bytes: 256 * 1024,
            max_quic_datagrams: 512,
            max_quic_bytes: 1024 * 1024,
            max_reads_per_call: 64,
        }
    }
}
impl DemuxConfig {
    pub fn validate(&self) -> Result<()> {
        if self.max_control_datagrams == 0
            || self.max_control_bytes < 2048
            || self.max_quic_datagrams == 0
            || self.max_quic_bytes < 64 * 1024
            || self.max_reads_per_call == 0
        {
            return Err(Error::Invalid("invalid demux config"));
        }
        Ok(())
    }
}

/// Nonsecret counters of a [`DemuxSocket`] pair.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DemuxStats {
    pub control_received: u64,
    pub quic_received: u64,
    /// Control datagrams dropped because the control queue was full.
    pub control_dropped: u64,
    /// QUIC datagrams dropped because the QUIC queue was full.
    pub quic_dropped: u64,
}

const SCRATCH_BYTES: usize = 64 * 1024;

struct Queue {
    items: VecDeque<(SocketAddr, Vec<u8>)>,
    bytes: usize,
    max_items: usize,
    max_bytes: usize,
}
impl Queue {
    fn new(max_items: usize, max_bytes: usize) -> Self {
        Self {
            items: VecDeque::new(),
            bytes: 0,
            max_items,
            max_bytes,
        }
    }
    fn push(&mut self, from: SocketAddr, datagram: &[u8]) -> bool {
        if self.items.len() >= self.max_items
            || datagram.len() > self.max_bytes.saturating_sub(self.bytes)
        {
            return false;
        }
        self.bytes += datagram.len();
        self.items.push_back((from, datagram.to_vec()));
        true
    }
    fn pop_into(&mut self, buffer: &mut [u8]) -> Option<(usize, SocketAddr)> {
        let (from, datagram) = self.items.pop_front()?;
        self.bytes -= datagram.len();
        let len = datagram.len().min(buffer.len());
        buffer[..len].copy_from_slice(&datagram[..len]);
        Some((len, from))
    }
}

struct Shared<S> {
    inner: S,
    control: Queue,
    quic: Queue,
    scratch: Vec<u8>,
    max_reads: usize,
    stats: DemuxStats,
}
impl<S: DatagramSocket> Shared<S> {
    /// Reads until a datagram for the requested side arrives; the other side's datagrams are
    /// queued for it (or dropped and counted when its queue is full).
    fn read(&mut self, buffer: &mut [u8], want_control: bool) -> io::Result<(usize, SocketAddr)> {
        let own = if want_control {
            &mut self.control
        } else {
            &mut self.quic
        };
        if let Some(received) = own.pop_into(buffer) {
            return Ok(received);
        }
        for _ in 0..self.max_reads {
            let (len, from) = match self.inner.recv_from(&mut self.scratch) {
                Ok(received) => received,
                Err(e) if is_transient(&e) && !want_control => return Err(e),
                Err(e) if is_transient(&e) => continue,
                Err(e) => return Err(e),
            };
            let datagram = &self.scratch[..len];
            let is_control = wire::is_control_datagram(datagram);
            if is_control {
                self.stats.control_received += 1;
            } else {
                self.stats.quic_received += 1;
            }
            if is_control == want_control {
                let len = len.min(buffer.len());
                buffer[..len].copy_from_slice(&self.scratch[..len]);
                return Ok((len, from));
            }
            let (queue, dropped) = if is_control {
                (&mut self.control, &mut self.stats.control_dropped)
            } else {
                (&mut self.quic, &mut self.stats.quic_dropped)
            };
            if !queue.push(from, datagram) {
                *dropped += 1;
            }
        }
        Err(io::ErrorKind::WouldBlock.into())
    }
}

fn lock<S>(shared: &Mutex<Shared<S>>) -> MutexGuard<'_, Shared<S>> {
    // State stays consistent across a panic elsewhere (every update is a whole push/pop).
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The QUIC half: a [`DatagramSocket`] that yields only non-control datagrams. Control
/// datagrams (first byte `0x01..=0x3F` followed by the 4-byte magic, see
/// [`crate::is_control_datagram`]) are diverted to the paired [`NatChannel`]. Hand this to
/// `QuicServer::with_socket`/`QuicClient::with_socket` and keep the channel to go on punching
/// for new peers or discovering addresses on the live socket.
pub struct DemuxSocket<S> {
    shared: Arc<Mutex<Shared<S>>>,
}

/// The NAT half of a [`DemuxSocket`]: a [`DatagramSocket`] that yields only control
/// datagrams. Drive [`crate::ControlAgent`]s over it with [`crate::drive_agents`], or wrap it
/// in a [`crate::RelaySocket`].
pub struct NatChannel<S> {
    shared: Arc<Mutex<Shared<S>>>,
}

impl<S: DatagramSocket> DemuxSocket<S> {
    /// Splits `inner` (nonblocking) into its QUIC and NAT halves.
    pub fn new(inner: S, config: DemuxConfig) -> Result<(Self, NatChannel<S>)> {
        config.validate()?;
        let shared = Arc::new(Mutex::new(Shared {
            inner,
            control: Queue::new(config.max_control_datagrams, config.max_control_bytes),
            quic: Queue::new(config.max_quic_datagrams, config.max_quic_bytes),
            scratch: vec![0; SCRATCH_BYTES],
            max_reads: config.max_reads_per_call,
            stats: DemuxStats::default(),
        }));
        Ok((
            Self {
                shared: shared.clone(),
            },
            NatChannel { shared },
        ))
    }
    pub fn stats(&self) -> DemuxStats {
        lock(&self.shared).stats
    }
}
impl<S: DatagramSocket> NatChannel<S> {
    pub fn stats(&self) -> DemuxStats {
        lock(&self.shared).stats
    }
}

impl<S: DatagramSocket> DatagramSocket for DemuxSocket<S> {
    fn send_to(&mut self, datagram: &[u8], destination: SocketAddr) -> io::Result<usize> {
        lock(&self.shared).inner.send_to(datagram, destination)
    }
    fn recv_from(&mut self, buffer: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        lock(&self.shared).read(buffer, false)
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        lock(&self.shared).inner.local_addr()
    }
}
impl<S: DatagramSocket> DatagramSocket for NatChannel<S> {
    fn send_to(&mut self, datagram: &[u8], destination: SocketAddr) -> io::Result<usize> {
        lock(&self.shared).inner.send_to(datagram, destination)
    }
    fn recv_from(&mut self, buffer: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        lock(&self.shared).read(buffer, true)
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        lock(&self.shared).inner.local_addr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;

    fn udp() -> UdpSocket {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        socket
    }
    fn control(n: u8) -> Vec<u8> {
        let mut d = vec![wire::kind::PUNCH_PROBE];
        d.extend_from_slice(&wire::MAGIC);
        d.push(n);
        d
    }

    #[test]
    fn each_half_sees_only_its_traffic_including_greased_quic() {
        let (mut quic, mut nat) = DemuxSocket::new(udp(), DemuxConfig::default()).unwrap();
        let target = quic.local_addr().unwrap();
        let sender = udp();
        // QUIC long header, short header, and short header with the fixed bit greased away.
        let quic_like: [&[u8]; 3] = [&[0xC3, 0, 0, 0, 1], &[0x41, 9, 9], &[0x01, 9, 9, 9, 9, 9]];
        for (i, d) in quic_like.iter().enumerate() {
            sender.send_to(d, target).unwrap();
            sender.send_to(&control(i as u8), target).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        let mut buf = [0; 64];
        let mut got_quic = Vec::new();
        while let Ok((len, _)) = quic.recv_from(&mut buf) {
            got_quic.push(buf[..len].to_vec());
        }
        let mut got_control = Vec::new();
        while let Ok((len, _)) = nat.recv_from(&mut buf) {
            got_control.push(buf[..len].to_vec());
        }
        assert_eq!(got_quic, quic_like.map(<[u8]>::to_vec));
        assert_eq!(got_control, (0..3).map(control).collect::<Vec<_>>());
        let stats = nat.stats();
        assert_eq!((stats.control_received, stats.quic_received), (3, 3));
    }

    #[test]
    fn queues_are_bounded() {
        let config = DemuxConfig {
            max_control_datagrams: 2,
            ..DemuxConfig::default()
        };
        let (mut quic, nat) = DemuxSocket::new(udp(), config).unwrap();
        let target = quic.local_addr().unwrap();
        let sender = udp();
        for i in 0..5 {
            sender.send_to(&control(i), target).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        let mut buf = [0; 64];
        assert_eq!(
            quic.recv_from(&mut buf).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(nat.stats().control_dropped, 3);
    }
}
