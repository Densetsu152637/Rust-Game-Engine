use crate::{Error, Limits, Message, PollBudget, Result};
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
};

/// Framing is transport-independent; production uses nonblocking TCP and tests inject partial I/O.
pub(crate) struct Framed<T> {
    io: T,
    input: Vec<u8>,
    expected: Option<usize>,
    output: VecDeque<Vec<u8>>,
    offset: usize,
    queued_bytes: usize,
    pending_error: Option<Error>,
}
impl<T: Read + Write> Framed<T> {
    pub(crate) fn new(io: T) -> Self {
        Self {
            io,
            input: Vec::new(),
            expected: None,
            output: VecDeque::new(),
            offset: 0,
            queued_bytes: 0,
            pending_error: None,
        }
    }
    pub(crate) fn encode(message: &Message, limits: &Limits) -> Result<Vec<u8>> {
        // Bounded writer avoids allocating an oversized JSON frame before checking the cap.
        let mut writer = CappedWriter {
            bytes: Vec::new(),
            cap: limits.max_frame_bytes,
        };
        serde_json::to_writer(&mut writer, message)
            .map_err(|_| Error::Invalid("frame exceeds cap or cannot serialize"))?;
        let mut frame = Vec::with_capacity(writer.bytes.len() + 4);
        frame.extend_from_slice(&(writer.bytes.len() as u32).to_be_bytes());
        frame.extend(writer.bytes);
        Ok(frame)
    }
    pub(crate) fn can_queue(&self, frame: &[u8], limits: &Limits) -> bool {
        self.output.len() < limits.max_queued_frames
            && frame.len() <= limits.max_queued_bytes.saturating_sub(self.queued_bytes)
    }
    pub(crate) fn queue(&mut self, frame: Vec<u8>, limits: &Limits) -> Result<()> {
        if !self.can_queue(&frame, limits) {
            return Err(Error::QueueFull);
        }
        self.queued_bytes += frame.len();
        self.output.push_back(frame);
        Ok(())
    }
    pub(crate) fn flush(&mut self, mut bytes: usize) -> Result<()> {
        while bytes > 0 {
            let Some(frame) = self.output.front() else {
                break;
            };
            let end = frame.len().min(self.offset.saturating_add(bytes));
            match self.io.write(&frame[self.offset..end]) {
                Ok(0) => return Err(Error::Disconnected),
                Ok(count) => {
                    bytes -= count;
                    self.offset += count;

                    if self.offset == frame.len() {
                        self.queued_bytes -= frame.len();
                        self.output.pop_front();
                        self.offset = 0;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    pub(crate) fn receive(&mut self, budget: PollBudget, limits: &Limits) -> Result<Vec<Message>> {
        if let Some(error) = self.pending_error.take() {
            return Err(error);
        }
        let mut messages = Vec::new();
        let mut bytes = budget.max_bytes;
        while messages.len() < budget.max_messages {
            let target = self.expected.unwrap_or(4);
            if self.input.len() == target {
                if self.expected.is_none() {
                    let length = u32::from_be_bytes([
                        self.input[0],
                        self.input[1],
                        self.input[2],
                        self.input[3],
                    ]) as usize;
                    if length == 0 || length > limits.max_frame_bytes {
                        return self.finish_before_error(
                            messages,
                            Error::Protocol("frame length exceeds cap"),
                        );
                    }
                    self.input.clear();
                    self.expected = Some(length);
                } else {
                    let message = match serde_json::from_slice(&self.input) {
                        Ok(message) => message,
                        Err(_) => {
                            return self
                                .finish_before_error(messages, Error::Protocol("malformed frame"));
                        }
                    };
                    messages.push(message);
                    self.input.clear();
                    self.expected = None;
                }
                continue;
            }
            if bytes == 0 {
                break;
            }
            let mut buffer = [0_u8; 4096];
            let count = (target - self.input.len()).min(buffer.len()).min(bytes);
            match self.io.read(&mut buffer[..count]) {
                Ok(0) => return self.finish_before_error(messages, Error::Disconnected),
                Ok(count) => {
                    self.input.extend_from_slice(&buffer[..count]);
                    bytes -= count;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => break,
                Err(e) => return self.finish_before_error(messages, e.into()),
            }
        }
        Ok(messages)
    }
    fn finish_before_error(
        &mut self,
        messages: Vec<Message>,
        error: Error,
    ) -> Result<Vec<Message>> {
        if messages.is_empty() {
            return Err(error);
        }
        self.pending_error = Some(error);
        Ok(messages)
    }
}
struct CappedWriter {
    bytes: Vec<u8>,
    cap: usize,
}
impl Write for CappedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.cap.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("frame cap"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Partial {
        incoming: VecDeque<u8>,
        outgoing: Vec<u8>,
        chunk: usize,
    }
    impl Read for Partial {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.incoming.is_empty() {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let n = bytes.len().min(self.chunk).min(self.incoming.len());
            for byte in &mut bytes[..n] {
                *byte = self.incoming.pop_front().unwrap();
            }
            Ok(n)
        }
    }
    impl Write for Partial {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let n = bytes.len().min(self.chunk);
            self.outgoing.extend_from_slice(&bytes[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn fragmented_frames_and_partial_writes_preserve_messages() {
        let limits = Limits::default();
        let frame = Framed::<Partial>::encode(
            &Message::Welcome {
                version: 1,
                peer_id: 42,
            },
            &limits,
        )
        .unwrap();
        let mut io = Framed::new(Partial {
            incoming: frame.clone().into(),
            chunk: 1,
            ..Default::default()
        });
        io.queue(frame.clone(), &limits).unwrap();
        let mut messages = Vec::new();
        for _ in 0..frame.len() {
            messages.extend(
                io.receive(
                    PollBudget {
                        max_messages: 1,
                        max_bytes: 1,
                        max_accepts: 0,
                    },
                    &limits,
                )
                .unwrap(),
            );
            io.flush(1).unwrap();
        }
        assert!(matches!(
            messages.as_slice(),
            [Message::Welcome { peer_id: 42, .. }]
        ));
        assert_eq!(io.io.outgoing, frame);
        assert_eq!(io.queued_bytes, 0);
    }
    #[test]
    fn rejects_huge_header_before_payload_allocation_and_caps_queues() {
        let limits = Limits {
            max_queued_frames: 1,
            ..Limits::default()
        };
        let mut io = Framed::new(Partial {
            incoming: u32::MAX.to_be_bytes().into(),
            chunk: 4,
            ..Default::default()
        });
        assert!(matches!(
            io.receive(PollBudget::default(), &limits),
            Err(Error::Protocol(_))
        ));
        assert_eq!(io.input.len(), 4);
        let frame = Framed::<Partial>::encode(
            &Message::Welcome {
                version: 1,
                peer_id: 42,
            },
            &limits,
        )
        .unwrap();
        io.queue(frame.clone(), &limits).unwrap();
        assert!(matches!(io.queue(frame, &limits), Err(Error::QueueFull)));
    }
}
