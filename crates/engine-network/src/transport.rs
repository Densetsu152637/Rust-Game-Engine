use crate::{Error, Result, SecurityLimits, security::verify_protocol};
use std::{
    io::{self, Read, Write},
    net::TcpStream,
    time::Instant,
};

pub(crate) enum Transport {
    Plain(TcpStream),
    Tls(Box<TlsTransport>),
}
pub(crate) struct TlsTransport {
    socket: TcpStream,
    pub(crate) connection: rustls::Connection,
    limits: SecurityLimits,
    started: Instant,
    handshake_bytes: usize,
    rate_started: Instant,
    received_bytes: usize,
    received_messages: usize,
    verified: bool,
}
impl Transport {
    pub(crate) fn configure_socket(&self) -> Result<()> {
        let socket = match self {
            Self::Plain(socket) => socket,
            Self::Tls(tls) => &tls.socket,
        };
        socket.set_nonblocking(true)?;
        socket.set_nodelay(true)?;
        Ok(())
    }
    pub(crate) fn tls(
        socket: TcpStream,
        mut connection: rustls::Connection,
        limits: SecurityLimits,
    ) -> Result<Self> {
        limits.validate()?;
        // Additional to the framed queue: a 64 KiB limit on each outgoing rustls buffer.
        connection.set_buffer_limit(Some(64 * 1024));
        Ok(Self::Tls(Box::new(TlsTransport {
            socket,
            connection,
            limits,
            started: Instant::now(),
            handshake_bytes: 0,
            rate_started: Instant::now(),
            received_bytes: 0,
            received_messages: 0,
            verified: false,
        })))
    }
    pub(crate) fn pump(&mut self, bytes: usize) -> Result<()> {
        if let Self::Tls(tls) = self {
            tls.pump(bytes)?;
        }
        Ok(())
    }
    pub(crate) fn ready(&self) -> bool {
        match self {
            Self::Plain(_) => true,
            Self::Tls(tls) => tls.verified,
        }
    }
    pub(crate) fn certificates(&self) -> Option<&[crate::CertificateDer<'static>]> {
        match self {
            Self::Plain(_) => None,
            Self::Tls(tls) => tls.connection.peer_certificates(),
        }
    }
    pub(crate) fn charge_messages(&mut self, count: usize) -> Result<()> {
        if let Self::Tls(tls) = self {
            tls.received_messages = tls.received_messages.saturating_add(count);
            if tls.received_messages > tls.limits.max_messages_per_second {
                return Err(Error::Protocol("message rate exceeded"));
            }
        }
        Ok(())
    }
}
impl TlsTransport {
    fn pump(&mut self, bytes: usize) -> Result<()> {
        if !self.verified && self.started.elapsed() >= self.limits.handshake_timeout {
            return Err(Error::Protocol("TLS authentication timeout"));
        }
        if self.rate_started.elapsed().as_secs() >= 1 {
            self.rate_started = Instant::now();
            self.received_bytes = 0;
            self.received_messages = 0;
        }
        let mut remaining = bytes;
        for _ in 0..self.limits.max_tls_steps_per_poll {
            if remaining == 0 || !self.connection.wants_read() {
                break;
            }
            let count = match self
                .connection
                .read_tls(&mut (&mut self.socket).take(remaining as u64))
            {
                Ok(0) => return Err(Error::Disconnected),
                Ok(n) => n,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    break;
                }
                Err(e) => return Err(e.into()),
            };
            remaining -= count;
            self.received_bytes = self.received_bytes.saturating_add(count);
            if self.received_bytes > self.limits.max_received_bytes_per_second {
                return Err(Error::Protocol("TLS receive rate exceeded"));
            }
            if !self.verified {
                self.handshake_bytes = self.handshake_bytes.saturating_add(count);
                if self.handshake_bytes > self.limits.max_handshake_bytes {
                    return Err(Error::Protocol("TLS handshake byte cap exceeded"));
                }
            }
            self.connection
                .process_new_packets()
                .map_err(|_| Error::Protocol("TLS authentication or record failed"))?;
            if !self.connection.is_handshaking() && !self.verified {
                verify_protocol(&self.connection)?;
                self.verified = true;
            }
        }
        let mut writer = LimitedWriter {
            inner: &mut self.socket,
            remaining: bytes,
        };
        for _ in 0..self.limits.max_tls_steps_per_poll {
            if writer.remaining == 0 || !self.connection.wants_write() {
                break;
            }
            match self.connection.write_tls(&mut writer) {
                Ok(0) => return Err(Error::Disconnected),
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}
struct LimitedWriter<'a> {
    inner: &'a mut TcpStream,
    remaining: usize,
}
impl Write for LimitedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let count = self
            .inner
            .write(&bytes[..bytes.len().min(self.remaining)])?;
        self.remaining -= count;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Read for Transport {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(socket) => socket.read(buffer),
            Self::Tls(tls) => tls.connection.reader().read(buffer),
        }
    }
}
impl Write for Transport {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(socket) => socket.write(bytes),
            Self::Tls(tls) => {
                let count = tls.connection.writer().write(bytes)?;
                if count == 0 && !bytes.is_empty() {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                Ok(count)
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
