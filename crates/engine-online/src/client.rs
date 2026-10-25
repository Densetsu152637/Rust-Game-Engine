//! Client-side driver: speaks the control protocol over any [`MessageEndpoint`].
use crate::{
    OnlineError,
    limits::OnlineLimits,
    protocol::{Blob, ClientBody, ClientEnvelope, PROTOCOL_VERSION, ServerBody, ServerEnvelope},
};
use engine_network::{Delivery, Lane, LinkEvent, MessageEndpoint, PollBudget, SERVER_PEER};

#[derive(Clone, Debug, PartialEq)]
pub enum ClientEvent {
    /// The transport admitted this client (the server accepted its certificate).
    Connected,
    Disconnected(String),
    /// Answer to request `id`.
    Response {
        id: u32,
        body: ServerBody,
    },
    /// Unsolicited server message (member joined, host changed, match found...).
    Notification(ServerBody),
}

pub struct OnlineClient<E: MessageEndpoint> {
    endpoint: E,
    limits: OnlineLimits,
    next_id: u32,
}
impl<E: MessageEndpoint> OnlineClient<E> {
    pub fn new(endpoint: E, limits: OnlineLimits) -> Result<Self, OnlineError> {
        limits.validate()?;
        Ok(Self {
            endpoint,
            limits,
            next_id: 1,
        })
    }
    pub fn endpoint(&self) -> &E {
        &self.endpoint
    }
    pub fn endpoint_mut(&mut self) -> &mut E {
        &mut self.endpoint
    }

    /// Sends one request and returns its ID; the answer arrives as [`ClientEvent::Response`].
    pub fn request(&mut self, body: ClientBody) -> Result<u32, OnlineError> {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        let payload = ClientEnvelope {
            v: PROTOCOL_VERSION,
            id,
            body,
        }
        .encode(&self.limits)?;
        self.endpoint.send(
            SERVER_PEER,
            Lane::SESSION,
            Delivery::ReliableOrdered,
            &payload,
        )?;
        Ok(id)
    }
    /// Authenticates with a connect token (must be the first request).
    pub fn hello(&mut self, token: &[u8]) -> Result<u32, OnlineError> {
        self.request(ClientBody::Hello {
            token: Blob(token.to_vec()),
        })
    }

    pub fn poll(&mut self) -> Result<Vec<ClientEvent>, OnlineError> {
        let mut out = Vec::new();
        for event in self.endpoint.poll(PollBudget::default())? {
            match event {
                LinkEvent::Authenticated(_) => {}
                LinkEvent::Connected(_) => out.push(ClientEvent::Connected),
                LinkEvent::Disconnected { reason, .. } => {
                    out.push(ClientEvent::Disconnected(reason));
                }
                LinkEvent::Message {
                    lane,
                    delivery,
                    payload,
                    ..
                } => {
                    if lane != Lane::SESSION || delivery != Delivery::ReliableOrdered {
                        continue;
                    }
                    match ServerEnvelope::decode(&payload, &self.limits) {
                        Ok(envelope) if envelope.id == 0 => {
                            out.push(ClientEvent::Notification(envelope.body));
                        }
                        Ok(envelope) => out.push(ClientEvent::Response {
                            id: envelope.id,
                            body: envelope.body,
                        }),
                        Err(_) => {
                            // A server that violates the protocol is not trusted further.
                            self.endpoint.disconnect(SERVER_PEER);
                            out.push(ClientEvent::Disconnected("protocol violation".into()));
                        }
                    }
                }
            }
        }
        Ok(out)
    }
}
