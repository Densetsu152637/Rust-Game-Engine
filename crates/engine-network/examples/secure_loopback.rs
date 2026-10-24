//! Synthetic in-memory identities for demonstration only. Never reuse these as deployed credentials.
use engine_network::{
    Client, ClientConfig, ClientSecurity, EntitySnapshot, InputCommand, PollBudget, PrivateKeyDer,
    Server, ServerConfig, ServerEvent, ServerSecurity, Snapshot,
};
use std::{
    thread,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server_id = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    let client_id = rcgen::generate_simple_self_signed(vec!["example-player".into()])?;
    let allowed = client_id.cert.der().clone();
    let server_security = ServerSecurity::new(
        vec![server_id.cert.der().clone()],
        PrivateKeyDer::Pkcs8(server_id.signing_key.serialize_der().into()),
        [allowed.clone()],
        move |_peer_id, verified_chain| verified_chain.first() == Some(&allowed),
    )?;
    let client_security = ClientSecurity::new(
        "localhost",
        [server_id.cert.der().clone()],
        vec![client_id.cert.der().clone()],
        PrivateKeyDer::Pkcs8(client_id.signing_key.serialize_der().into()),
    )?;
    let mut server = Server::bind_secure(ServerConfig::default(), server_security)?;
    let mut client =
        Client::connect_secure(ClientConfig::new(server.local_addr()?), client_security)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !client.is_connected() {
        server.poll(PollBudget::default())?;
        client.poll(PollBudget::default())?;
        if Instant::now() >= deadline {
            return Err("authentication timed out".into());
        }
        thread::sleep(Duration::from_millis(1));
    }
    let peer = client.peer_id().ok_or("missing authenticated peer")?;
    server.assign_owner("hero", peer)?;
    client.send_input(InputCommand {
        tick: 1,
        entity_id: "hero".into(),
        movement: [1.0, 0.0, 0.0],
        buttons: 0,
    })?;
    let mut position = 0.0;
    while client.state().latest().is_none() {
        client.poll(PollBudget::default())?;
        for event in server.poll(PollBudget::default())? {
            if let ServerEvent::Input { command, .. } = event {
                position += command.movement[0];
                server.publish_snapshot(Snapshot {
                    tick: 1,
                    entities: vec![EntitySnapshot {
                        id: "hero".into(),
                        translation: [position, 0.0, 0.0],
                        rotation: [0.0, 0.0, 0.0, 1.0],
                        scale: [1.0; 3],
                        velocity: [0.0; 3],
                    }],
                })?;
            }
        }
        if Instant::now() >= deadline {
            return Err("replication timed out".into());
        }
        thread::sleep(Duration::from_millis(1));
    }
    println!(
        "TLS 1.3 authenticated peer {peer}; authoritative hero position {position} replicated."
    );
    Ok(())
}
