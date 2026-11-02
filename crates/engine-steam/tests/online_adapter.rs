#![cfg(feature = "online")]

mod common;

use engine_online::protocol::{LeaveReason, ServerBody};
use engine_steam::*;
use std::sync::{Arc, Mutex};

struct Shared {
    fake: Arc<Mutex<FakeBackend>>,
}
impl SteamBackend for Shared {
    fn status(&self) -> SteamStatus {
        self.fake.lock().unwrap().status()
    }
    fn lobby_backend(&mut self) -> Option<&mut dyn LobbyBackend> {
        Some(self)
    }
}
common::forward_lobby!(Shared);

#[test]
fn lobby_events_become_online_notifications() {
    let fake = Arc::new(Mutex::new(FakeBackend::ready(AppId(480), "tester")));
    let session = SteamSession::new(Box::new(Shared { fake: fake.clone() }));
    let mut lobbies = SteamLobbies::new(session).unwrap();
    let owner = SteamId(76_561_198_000_000_100);
    let guest = SteamId(76_561_198_000_000_101);
    let id = fake
        .lock()
        .unwrap()
        .sim_add_lobby(owner, 4, &[("mode", "ctf"), (HOST_KEY, "1")]);
    lobbies.join(id).unwrap();
    let mut adapter = OnlineAdapter::new();
    let convert = |lobbies: &mut SteamLobbies, adapter: &mut OnlineAdapter| -> Vec<ServerBody> {
        let events = lobbies.poll();
        events
            .iter()
            .filter_map(|e| adapter.convert(lobbies, e))
            .collect()
    };
    // Joined has no notification form.
    assert!(convert(&mut lobbies, &mut adapter).is_empty());
    let session_id = OnlineAdapter::session_id(id);
    assert!(session_id < 1 << 53);

    fake.lock().unwrap().sim_lobby_join(id, guest);
    let bodies = convert(&mut lobbies, &mut adapter);
    assert!(matches!(
        bodies.as_slice(),
        [ServerBody::MemberJoined { session_id: s, member }]
            if *s == session_id && member.player == guest.0.to_string() && !member.host
    ));

    fake.lock().unwrap().sim_lobby_set_data(id, "map", "dust");
    let bodies = convert(&mut lobbies, &mut adapter);
    let [ServerBody::AttributesChanged { attributes, .. }] = bodies.as_slice() else {
        panic!("{bodies:?}");
    };
    assert_eq!(attributes.get("map").map(String::as_str), Some("dust"));
    assert!(
        !attributes.contains_key(HOST_KEY),
        "engine keys are internal"
    );

    // Reserved connect-identity keys produce no attribute notification.
    fake.lock().unwrap().sim_lobby_set_data(id, HOST_KEY, "2");
    assert!(convert(&mut lobbies, &mut adapter).is_empty());

    fake.lock().unwrap().sim_lobby_leave(id, owner);
    let bodies = convert(&mut lobbies, &mut adapter);
    assert!(bodies.iter().any(|b| matches!(
        b,
        ServerBody::MemberLeft { player, reason: LeaveReason::Left, .. } if *player == owner.0.to_string()
    )));
    assert!(bodies.iter().any(|b| matches!(
        b,
        ServerBody::HostChanged { epoch: 1, host, you_are_host: true, .. } if *host == lobbies.me().0.to_string()
    )));

    lobbies.leave(id).unwrap();
    let bodies = convert(&mut lobbies, &mut adapter);
    assert!(matches!(bodies.as_slice(), [ServerBody::Left { session_id: s }] if *s == session_id));
}
