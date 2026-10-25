//! Delta-mode networking over real loopback sockets (`127.0.0.1:0`) with synthetic in-memory
//! identities: QUIC and TCP transports, replication, relevancy, RPC, simulation, lag
//! compensation, voice and telemetry.
use engine_net_quic::{
    QuicClient, QuicClientConfig, QuicClientSecurity, QuicServer, QuicServerConfig,
    QuicServerSecurity,
};
use engine_network::{CertificateDer, NetworkConditions, PrivateKeyDer};
use engine_project::{networking::*, *};
use serde_json::{Value, json};
use std::{
    fs,
    net::SocketAddr,
    thread,
    time::{Duration, Instant},
};

const HERO: &str = "hero";
const ROCK: &str = "rock";

fn scene_entity(id: &str, authority: &str, position: [f32; 3], extra: Value) -> Value {
    let mut custom = json!({"engine.NetworkReplica":{"version":1,"data":{"authority":authority}}});
    for (name, data) in extra.as_object().cloned().unwrap_or_default() {
        custom[name] = json!({"version":1,"data":data});
    }
    json!({"id":id,"name":id,"components":{"Transform":{"position":position},"custom":custom}})
}

fn project(entities: Vec<Value>, session: Option<Value>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut entities = entities;
    if let Some(session) = session {
        entities.push(
            json!({"id":"network","name":"network","components":{"custom":{
            "engine.NetworkSession":{"version":1,"data":session}}}}),
        );
    }
    fs::write(
        dir.path().join("scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"main","entities":entities})).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.path().join("project.json"),
        serde_json::to_vec(
            &json!({"schema":1,"name":"net","startup_scene":"scene.json","assets":[]}),
        )
        .unwrap(),
    )
    .unwrap();
    dir
}
fn runtime(dir: &tempfile::TempDir) -> Runtime {
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.set_user_data_root(dir.path().join("data")).unwrap();
    runtime
}
fn step(runtime: &mut Runtime) {
    runtime.tick(1.0 / 60.0, InputSnapshot::default()).unwrap();
}
fn until(what: &str, mut test: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !test() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(1));
    }
}
/// Everything that was ever delivered, since records only keep the last dispatch.
#[derive(Default)]
struct Log(Vec<Value>);
impl Log {
    fn absorb(&mut self, runtime: &Runtime) {
        for record in runtime.snapshot().events {
            self.0.push(serde_json::to_value(record).unwrap());
        }
    }
    fn find(&self, name: &str) -> Option<&Value> {
        self.0.iter().find(|e| e["name"] == name)
    }
}
fn identity(name: &str) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let key = rcgen::generate_simple_self_signed(vec![name.into()]).unwrap();
    (
        key.cert.der().clone(),
        PrivateKeyDer::Pkcs8(key.signing_key.serialize_der().into()),
    )
}
struct Quic {
    server: QuicServer,
    clients: Vec<QuicClient>,
}
fn quic(clients: usize) -> Quic {
    let (server_cert, server_key) = identity("localhost");
    let players: Vec<_> = (0..clients)
        .map(|i| identity(&format!("player{i}")))
        .collect();
    let mut roots: Vec<_> = players.iter().map(|(c, _)| c.clone()).collect();
    if roots.is_empty() {
        roots.push(identity("unused").0);
    }
    let trusted = roots.clone();
    let security = QuicServerSecurity::new(
        vec![server_cert.clone()],
        server_key,
        roots,
        move |_, chain| {
            chain
                .first()
                .is_some_and(|c| trusted.contains(&c.clone().into_owned()))
        },
    )
    .unwrap();
    let server = QuicServer::bind(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        QuicServerConfig::default(),
        security,
    )
    .unwrap();
    let address = server.local_addr().unwrap();
    let clients = players
        .into_iter()
        .map(|(cert, key)| {
            QuicClient::connect(
                address,
                QuicClientConfig::default(),
                QuicClientSecurity::new("localhost", [server_cert.clone()], vec![cert], key)
                    .unwrap(),
            )
            .unwrap()
        })
        .collect();
    Quic { server, clients }
}

fn delta(settings: NetworkSettings) -> NetworkSettings {
    NetworkSettings {
        mode: NetworkMode::Delta,
        ..settings
    }
}
fn world_status(runtime: &Runtime) -> NetworkStatus {
    status(runtime.app().world()).unwrap()
}
fn set_position(runtime: &mut Runtime, id: &str, position: [f32; 3]) {
    let entity = runtime.entity(id).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .get_mut::<Transform>(entity)
        .unwrap()
        .position = position;
}

struct Pair {
    _dirs: [tempfile::TempDir; 2],
    server: Runtime,
    client: Runtime,
    server_log: Log,
    client_log: Log,
}
impl Pair {
    fn step(&mut self) {
        step(&mut self.client);
        step(&mut self.server);
        self.server_log.absorb(&self.server);
        self.client_log.absorb(&self.client);
    }
    fn until(&mut self, what: &str, mut test: impl FnMut(&Runtime, &Runtime) -> bool) {
        self.until_logs(what, |s, c, _, _| test(s, c));
    }
    /// Like `until`, with the accumulated server and client event logs.
    fn until_logs(
        &mut self,
        what: &str,
        mut test: impl FnMut(&Runtime, &Runtime, &Log, &Log) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.step();
            if test(
                &self.server,
                &self.client,
                &self.server_log,
                &self.client_log,
            ) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(1));
        }
    }
}

fn server_entities(rule: Value) -> Vec<Value> {
    vec![
        scene_entity(
            HERO,
            "server",
            [0.0, 0.0, 0.0],
            json!({"engine.NetworkReplication": rule}),
        ),
        scene_entity(
            ROCK,
            "server",
            [500.0, 0.0, 0.0],
            json!({"engine.NetworkReplication": {"relevance":"spatial"}}),
        ),
    ]
}
fn client_entities() -> Vec<Value> {
    vec![
        scene_entity(HERO, "remote", [9.0, 9.0, 9.0], json!({})),
        scene_entity(ROCK, "remote", [9.0, 9.0, 9.0], json!({})),
    ]
}

/// A connected QUIC pair in delta mode; `hero_rule` is the hero's `engine.NetworkReplication`.
fn quic_pair(server_settings: NetworkSettings, client_settings: NetworkSettings) -> Pair {
    let server_dir = project(server_entities(json!({"relevance":"always"})), None);
    let client_dir = project(client_entities(), None);
    let mut server = runtime(&server_dir);
    let mut client = runtime(&client_dir);
    let Quic {
        server: endpoint,
        mut clients,
    } = quic(1);
    attach_endpoint_server(&mut server, Box::new(endpoint), delta(server_settings)).unwrap();
    attach_endpoint_client(
        &mut client,
        Box::new(clients.remove(0)),
        delta(client_settings),
    )
    .unwrap();
    let mut pair = Pair {
        _dirs: [server_dir, client_dir],
        server,
        client,
        server_log: Log::default(),
        client_log: Log::default(),
    };
    pair.until("QUIC connection", |s, c| {
        world_status(c).connected && world_status(s).connected
    });
    pair
}

#[test]
fn quic_delta_replicates_movement_hides_far_entities_and_round_trips_rpcs() {
    let mut pair = quic_pair(NetworkSettings::default(), NetworkSettings::default());
    let peer = world_status(&pair.server).peers[0];
    assign_owner(pair.server.app_mut().world_mut(), HERO, peer).unwrap();
    set_peer_view(pair.server.app_mut().world_mut(), peer, None, 50.0).unwrap();

    // Authoritative movement replicates; the far entity stays hidden.
    set_position(&mut pair.server, HERO, [3.0, 1.0, -2.0]);
    pair.until("hero position", |_, c| {
        c.position(HERO).is_some_and(|p| p == [3.0, 1.0, -2.0])
    });
    assert!(
        pair.client.entity(ROCK).is_none(),
        "far rock must stay hidden"
    );
    assert!(pair.client_log.find("network.entity_spawned").is_some());
    for x in 1..=20 {
        set_position(&mut pair.server, HERO, [x as f32, 0.0, 0.0]);
        pair.step();
    }
    pair.until("hero follows", |_, c| {
        c.position(HERO).is_some_and(|p| p[0] == 20.0)
    });

    // Moving the rock into the view sphere spawns it; moving it away despawns it again.
    set_position(&mut pair.server, ROCK, [25.0, 0.0, 0.0]);
    pair.until("rock enters view", |_, c| c.entity(ROCK).is_some());
    set_position(&mut pair.server, ROCK, [500.0, 0.0, 0.0]);
    set_position(&mut pair.server, HERO, [0.0, 0.0, 0.0]);
    pair.until("rock leaves view", |_, c| c.entity(ROCK).is_none());
    assert!(pair.client_log.find("network.entity_despawned").is_some());

    // RPC round trip with ownership enforcement.
    rpc_to_server(
        pair.client.app_mut().world_mut(),
        ClientRpcTarget::Owned(HERO),
        "fire",
        &json!({"weapon": 2}),
    )
    .unwrap();
    pair.until_logs("server rpc event", |_, _, s, _| {
        s.find("network.rpc").is_some()
    });
    let call = pair.server_log.find("network.rpc").unwrap().clone();
    assert_eq!(call["payload"]["name"], "fire");
    assert_eq!(call["payload"]["payload"]["weapon"], 2);
    assert_eq!(call["payload"]["entity_id"], HERO);
    assert_eq!(call["target"], HERO);
    let recipients = rpc_to_clients(
        pair.server.app_mut().world_mut(),
        RpcRecipient::Owner,
        "hit",
        Some(HERO),
        &json!({"damage": 7}),
    )
    .unwrap();
    assert_eq!(recipients, 1);
    pair.until_logs("client rpc event", |_, _, _, c| {
        c.find("network.rpc").is_some()
    });
    assert_eq!(
        pair.client_log.find("network.rpc").unwrap()["payload"]["payload"]["damage"],
        7
    );
    // Oversized payloads and bad names never leave the process.
    assert!(
        rpc_to_server(
            pair.client.app_mut().world_mut(),
            ClientRpcTarget::Session,
            "big",
            &json!({"blob": "x".repeat(900)}),
        )
        .is_err()
    );
    assert!(
        rpc_to_server(
            pair.client.app_mut().world_mut(),
            ClientRpcTarget::Session,
            "",
            &json!({})
        )
        .is_err()
    );
    // An entity the client cannot see cannot be targeted.
    assert!(
        rpc_to_server(
            pair.client.app_mut().world_mut(),
            ClientRpcTarget::Entity(ROCK),
            "poke",
            &json!({}),
        )
        .is_err()
    );
}

#[test]
fn input_flows_over_the_input_lane_with_ownership_and_replay_checks() {
    let mut pair = quic_pair(NetworkSettings::default(), NetworkSettings::default());
    let peer = world_status(&pair.server).peers[0];
    send_input(
        pair.client.app_mut().world_mut(),
        engine_network::InputCommand {
            tick: 1,
            entity_id: HERO.into(),
            movement: [1.0, 0.0, 0.0],
            buttons: 3,
        },
    )
    .unwrap();
    pair.until_logs("input rejected without ownership", |_, _, s, _| {
        s.find("network.input_rejected").is_some()
    });
    assign_owner(pair.server.app_mut().world_mut(), HERO, peer).unwrap();
    send_input(
        pair.client.app_mut().world_mut(),
        engine_network::InputCommand {
            tick: 2,
            entity_id: HERO.into(),
            movement: [0.5, 0.0, -1.0],
            buttons: 1,
        },
    )
    .unwrap();
    pair.until_logs("input accepted", |_, _, s, _| {
        s.find("network.input").is_some()
    });
    let input = pair.server_log.find("network.input").unwrap();
    assert_eq!(input["payload"]["buttons"], 1);
    assert_eq!(input["payload"]["movement"][2], -1.0);
    assert_eq!(world_status(&pair.server).accepted_inputs, 1);
    // The client refuses a non-increasing tick locally.
    assert!(
        send_input(
            pair.client.app_mut().world_mut(),
            engine_network::InputCommand {
                tick: 2,
                entity_id: HERO.into(),
                movement: [0.0; 3],
                buttons: 0,
            },
        )
        .is_err()
    );
}

#[test]
fn simulated_loss_latency_and_jitter_still_converge() {
    let conditions = NetworkConditions {
        loss: 0.3,
        latency_ms: 15,
        jitter_ms: 10,
        duplicate: 0.1,
        reorder: 0.1,
        seed: 7,
        ..NetworkConditions::default()
    };
    let settings = NetworkSettings {
        conditions: Some(conditions.clone()),
        ..NetworkSettings::default()
    };
    let mut pair = quic_pair(settings.clone(), settings);
    assert_eq!(world_status(&pair.server).conditions, Some(conditions));
    for x in 1..=60 {
        set_position(&mut pair.server, HERO, [x as f32, 0.0, 0.5]);
        pair.step();
    }
    pair.until("convergence under loss", |_, c| {
        c.position(HERO).is_some_and(|p| p == [60.0, 0.0, 0.5])
    });
    // Removing the simulation at runtime keeps the session usable.
    set_conditions(pair.server.app_mut().world_mut(), None).unwrap();
    set_conditions(pair.client.app_mut().world_mut(), None).unwrap();
    assert!(world_status(&pair.server).conditions.is_none());
    set_position(&mut pair.server, HERO, [1.0, 2.0, 3.0]);
    pair.until("convergence after clearing", |_, c| {
        c.position(HERO).is_some_and(|p| p == [1.0, 2.0, 3.0])
    });
    assert!(
        set_conditions(
            pair.server.app_mut().world_mut(),
            Some(NetworkConditions {
                loss: 2.0,
                ..NetworkConditions::default()
            })
        )
        .is_err()
    );
}

#[test]
fn telemetry_reports_transport_links_and_replication_within_bounds() {
    let mut pair = quic_pair(NetworkSettings::default(), NetworkSettings::default());
    for x in 1..=30 {
        set_position(&mut pair.server, HERO, [x as f32, 0.0, 0.0]);
        pair.step();
    }
    let report = world_status(&pair.server);
    assert_eq!(report.mode, NetworkMode::Delta);
    assert_eq!(report.transport, Some(engine_network::TransportKind::Quic));
    assert_eq!(
        report.security,
        engine_network::TransportSecurity::MutualTls13
    );
    assert_eq!(report.peer_count, 1);
    assert_eq!(report.links.len(), 1);
    assert!(report.links[0].link.sent_bytes > 0 && report.links[0].link.received_bytes > 0);
    let replication = report.replication.as_ref().unwrap();
    assert_eq!(replication.schema_hash.len(), 16);
    assert!(replication.entities >= 2);
    let peer = &replication.peers[0];
    assert!(peer.bytes_total > 0 && peer.packets_total > 0);
    assert!(peer.entities_relevant >= 1);
    assert!(peer.compression_ratio > 0.0);
    let client = world_status(&pair.client);
    assert_eq!(client.role, "client");
    assert!(
        client
            .replication
            .as_ref()
            .unwrap()
            .client
            .as_ref()
            .unwrap()
            .packets_received
            > 0
    );
    let json = serde_json::to_string(&pair.server.snapshot()).unwrap();
    assert!(json.len() < 64 * 1024, "snapshot is {} bytes", json.len());
    assert!(serde_json::to_value(&report).unwrap()["links"][0]["rtt_ms"].is_number());
    assert!(!json.contains("certificate"));
}

#[test]
fn reported_peers_are_capped() {
    let server_dir = project(server_entities(json!({})), None);
    let mut server = runtime(&server_dir);
    let Quic {
        server: endpoint,
        clients,
    } = quic(2);
    attach_endpoint_server(
        &mut server,
        Box::new(endpoint),
        NetworkSettings {
            max_reported_peers: 1,
            ..NetworkSettings::default()
        },
    )
    .unwrap();
    let dirs: Vec<_> = (0..2).map(|_| project(client_entities(), None)).collect();
    let mut runtimes: Vec<Runtime> = dirs.iter().map(runtime).collect();
    for (rt, client) in runtimes.iter_mut().zip(clients) {
        attach_endpoint_client(rt, Box::new(client), NetworkSettings::default()).unwrap();
    }
    until("two peers", || {
        step(&mut server);
        runtimes.iter_mut().for_each(step);
        world_status(&server).peer_count == 2
    });
    let report = world_status(&server);
    assert_eq!(report.peers.len(), 1);
    assert_eq!(report.links.len(), 1);
    assert!(report.replication.unwrap().peers_truncated);
}

#[test]
fn priority_rates_and_state_properties_replicate_with_events() {
    let settings = NetworkSettings {
        state_properties: vec![
            StateProperty {
                name: "health".into(),
                kind: StateKind::I32,
            },
            StateProperty {
                name: "title".into(),
                kind: StateKind::String,
            },
        ],
        property_events: true,
        ..NetworkSettings::default()
    };
    let server_dir = project(
        vec![scene_entity(
            HERO,
            "server",
            [0.0; 3],
            json!({"engine.NetworkReplication": {"priority": 4.0, "max_update_hz": 30.0}}),
        )],
        None,
    );
    let client_dir = project(
        vec![scene_entity(HERO, "remote", [0.0; 3], json!({}))],
        None,
    );
    let (mut server, mut client) = (runtime(&server_dir), runtime(&client_dir));
    let Quic {
        server: endpoint,
        mut clients,
    } = quic(1);
    attach_endpoint_server(&mut server, Box::new(endpoint), settings.clone()).unwrap();
    attach_endpoint_client(&mut client, Box::new(clients.remove(0)), settings).unwrap();
    let mut pair = Pair {
        _dirs: [server_dir, client_dir],
        server,
        client,
        server_log: Log::default(),
        client_log: Log::default(),
    };
    // The initial state travels with the spawn; property events need an acknowledged spawn.
    pair.until("spawn acknowledged", |s, c| {
        c.entity(HERO).is_some()
            && world_status(s)
                .replication
                .is_some_and(|r| r.peers.first().is_some_and(|p| p.packets_acked > 0))
    });
    set_state(
        pair.server.app_mut().world_mut(),
        HERO,
        "health",
        &json!(75),
    )
    .unwrap();
    set_state(
        pair.server.app_mut().world_mut(),
        HERO,
        "title",
        &json!("Knight"),
    )
    .unwrap();
    assert!(
        set_state(
            pair.server.app_mut().world_mut(),
            HERO,
            "health",
            &json!("bad")
        )
        .is_err()
    );
    assert!(set_state(pair.server.app_mut().world_mut(), HERO, "mana", &json!(1)).is_err());
    pair.until("state", |_, c| {
        state(c.app().world(), HERO, "health") == Some(json!(75))
    });
    assert_eq!(
        state(pair.client.app().world(), HERO, "title"),
        Some(json!("Knight"))
    );
    assert_eq!(
        state(pair.server.app().world(), HERO, "health"),
        Some(json!(75))
    );
    let change = pair
        .client_log
        .0
        .iter()
        .find(|e| e["name"] == "network.property_changed" && e["payload"]["property"] == "health")
        .expect("state change event");
    assert_eq!(change["payload"]["new"], 75);
    set_position(&mut pair.server, HERO, [4.0, 0.0, 0.0]);
    pair.until_logs("pose property event", |_, _, _, c| {
        c.0.iter().any(|e| {
            e["name"] == "network.property_changed" && e["payload"]["property"] == "position"
        })
    });
}

#[test]
fn lag_compensated_raycast_hits_the_rewound_pose_and_clamps_claims() {
    let dir = project(
        vec![scene_entity(
            HERO,
            "server",
            [0.0; 3],
            json!({"engine.NetworkHitboxes": {"layers": 2, "hitboxes": [{"shape":"sphere","radius":0.5,"part":7}]}}),
        )],
        None,
    );
    let mut server = runtime(&dir);
    let Quic {
        server: endpoint, ..
    } = quic(0);
    attach_endpoint_server(
        &mut server,
        Box::new(endpoint),
        NetworkSettings {
            lag_rewind_ticks: 4,
            history_ticks: 32,
            ..NetworkSettings::default()
        },
    )
    .unwrap();
    for tick in 1..=8 {
        set_position(&mut server, HERO, [tick as f32, 0.0, 0.0]);
        step(&mut server);
    }
    let world = server.app_mut().world_mut();
    let ray = |x: f32| predict::Ray {
        origin: glam::Vec3::new(x, 0.0, -10.0),
        direction: glam::Vec3::Z,
        max_distance: 100.0,
    };
    let query = |view_tick: f64, x: f32| LagRaycast {
        shooter: 99,
        view_tick,
        ray: ray(x),
        layer_mask: 2,
    };
    // At tick 8 the hero stands at x = 8; a shot at x = 6 only hits the pose of tick 6.
    let now = lag_compensated_raycast(world, &query(8.0, 6.0)).unwrap();
    assert!(now.hit.is_none(), "{now:?}");
    let rewound = lag_compensated_raycast(world, &query(6.0, 6.0)).unwrap();
    let hit = rewound.hit.expect("rewound hit");
    assert_eq!((hit.entity_id.as_str(), hit.part), (HERO, 7));
    assert!((hit.distance - 9.5).abs() < 1e-3 && !rewound.clamped);
    // Interpolated view ticks blend poses.
    let between = lag_compensated_raycast(world, &query(6.5, 6.5)).unwrap();
    assert!(between.hit.is_some());
    // A claim older than lag_rewind_ticks is clamped to tick 4 and therefore misses x = 1.
    let clamped = lag_compensated_raycast(world, &query(1.0, 1.0)).unwrap();
    assert!(clamped.clamped && clamped.rewind_tick == 4.0 && clamped.hit.is_none());
    // Layers filter, and future claims are refused.
    let masked = lag_compensated_raycast(
        world,
        &LagRaycast {
            layer_mask: 1,
            ..query(6.0, 6.0)
        },
    )
    .unwrap();
    assert!(masked.hit.is_none());
    assert!(lag_compensated_raycast(world, &query(100.0, 6.0)).is_err());
    assert!(lag_compensated_raycast(world, &query(f64::NAN, 6.0)).is_err());
    let (_, overlaps) =
        lag_compensated_overlap_sphere(world, 99, 6.0, [6.0, 0.0, 0.0], 0.2, 2).unwrap();
    assert_eq!(overlaps, vec![(HERO.to_owned(), 7)]);
    // The shooter's own entities are ignored.
    step(&mut server);
    let report = world_status(&server).lag_comp.unwrap();
    assert!(report.queries >= 5 && report.rejected_queries >= 2 && report.ticks_recorded <= 32);
    assert_eq!(report.entities, 1);
}

#[test]
fn voice_frames_route_between_runtimes_by_proximity() {
    let voice = |distance: f32| NetworkSettings {
        voice: Some(VoiceSettings {
            max_distance: distance,
            ..VoiceSettings::default()
        }),
        ..NetworkSettings::default()
    };
    let speaker = |id: &str, x: f32| {
        scene_entity(
            id,
            "server",
            [x, 0.0, 0.0],
            json!({"engine.NetworkVoice": {"channel": 1}}),
        )
    };
    let server_dir = project(
        vec![speaker("a", 0.0), speaker("b", 10.0), speaker("c", 400.0)],
        None,
    );
    let mut server = runtime(&server_dir);
    let Quic {
        server: endpoint,
        clients,
    } = quic(3);
    attach_endpoint_server(&mut server, Box::new(endpoint), voice(50.0)).unwrap();
    let dirs: Vec<_> = (0..3).map(|_| project(vec![], None)).collect();
    let mut peers: Vec<Runtime> = dirs.iter().map(runtime).collect();
    for (rt, client) in peers.iter_mut().zip(clients) {
        attach_endpoint_client(rt, Box::new(client), voice(50.0)).unwrap();
    }
    // Clients are stepped one at a time so peer IDs map to speakers in order.
    for (index, id) in ["a", "b", "c"].iter().enumerate() {
        until("peer connected", || {
            step(&mut server);
            peers[..=index].iter_mut().for_each(step);
            world_status(&server).peer_count > index
        });
        let peer = world_status(&server).peers[index];
        assign_owner(server.app_mut().world_mut(), id, peer).unwrap();
    }
    let granted = world_status(&server).peers;
    until("clients connected", || {
        step(&mut server);
        peers.iter_mut().for_each(step);
        peers.iter().all(|p| world_status(p).connected)
    });
    let frame = vec![1000i16; 320];
    let speak = |server: &mut Runtime, peers: &mut Vec<Runtime>, frames: usize| {
        let mut heard = vec![0usize; 3];
        for _ in 0..frames {
            send_voice_frame(peers[0].app_mut().world_mut(), &frame, false).unwrap();
            step(server);
            peers.iter_mut().for_each(step);
            for (index, p) in peers.iter_mut().enumerate() {
                for output in pull_voice(p.app_mut().world_mut()) {
                    if !output.concealed {
                        assert!(output.pcm.iter().all(|s| *s == 1000));
                        heard[index] += 1;
                    }
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        heard
    };
    // `a` speaks: `b` is within 50 m, `c` is 400 m away and `a` never hears itself.
    let heard = speak(&mut server, &mut peers, 40);
    assert_eq!(heard[0], 0, "a speaker never hears itself: {heard:?}");
    assert!(heard[1] > 0, "the neighbour hears the frames: {heard:?}");
    assert_eq!(heard[2], 0, "the far listener hears nothing: {heard:?}");
    // The speaker's own proximity_distance can only narrow the hearing range.
    let a = server.entity("a").unwrap();
    server
        .app_mut()
        .world_mut()
        .get_mut::<NetworkVoice>(a)
        .unwrap()
        .proximity_distance = 5.0;
    step(&mut server);
    let _ = speak(&mut server, &mut peers, 10);
    let narrowed = speak(&mut server, &mut peers, 20);
    assert_eq!(
        narrowed[1], 0,
        "narrowed to 5 m, b at 10 m is silent: {narrowed:?}"
    );
    let report = world_status(&server).voice.unwrap();
    assert!(report.frames_in >= 40 && report.frames_out > 0 && report.filtered > 0);
    assert_eq!(world_status(&peers[0]).voice.unwrap().frames_sent, 70);
    assert!(set_voice_muted(server.app_mut().world_mut(), granted[1], granted[0], true).is_ok());
    assert!(send_voice_frame(server.app_mut().world_mut(), &frame, false).is_err());
    assert!(send_voice_frame(peers[0].app_mut().world_mut(), &frame[..10], false).is_err());
}

#[test]
fn tcp_endpoints_run_delta_mode_and_the_legacy_mode_is_unchanged() {
    use engine_network::{Client, ClientConfig, Server, ServerConfig};
    let server_dir = project(server_entities(json!({})), None);
    let client_dir = project(client_entities(), None);
    let (mut server, mut client) = (runtime(&server_dir), runtime(&client_dir));
    let endpoint = Server::bind(ServerConfig::default()).unwrap();
    let remote = Client::connect(ClientConfig::new(endpoint.local_addr().unwrap())).unwrap();
    let settings = NetworkSettings {
        mode: NetworkMode::Delta,
        ..NetworkSettings::default()
    };
    attach_server(&mut server, endpoint, settings.clone()).unwrap();
    attach_client(&mut client, remote, settings).unwrap();
    until("tcp connected", || {
        step(&mut server);
        step(&mut client);
        world_status(&client).connected
    });
    set_position(&mut server, HERO, [5.0, 0.0, 0.0]);
    until("tcp delta replication", || {
        step(&mut server);
        step(&mut client);
        client.position(HERO) == Some([5.0, 0.0, 0.0])
    });
    let report = world_status(&server);
    assert_eq!(report.transport, Some(engine_network::TransportKind::Tcp));
    assert_eq!(
        report.security,
        engine_network::TransportSecurity::TrustedLanPlaintext
    );
    assert_eq!(report.mode, NetworkMode::Delta);
}

#[test]
fn lane_payloads_carry_prediction_and_application_traffic() {
    let mut pair = quic_pair(NetworkSettings::default(), NetworkSettings::default());
    let peer = world_status(&pair.server).peers[0];
    send_lane_payload(
        pair.client.app_mut().world_mut(),
        0,
        9,
        engine_network::Delivery::ReliableOrdered,
        b"app payload",
    )
    .unwrap();
    let mut received = Vec::new();
    while received.is_empty() {
        pair.step();
        received.extend(take_lane_payloads(pair.server.app_mut().world_mut()));
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(received[0].lane, 9);
    assert_eq!(received[0].peer, peer);
    assert_eq!(received[0].bytes, b"app payload");
    // Engine lanes cannot be hijacked.
    for lane in [1u8, 2, 4, 16] {
        assert!(
            send_lane_payload(
                pair.client.app_mut().world_mut(),
                0,
                lane,
                engine_network::Delivery::Unreliable,
                b"x"
            )
            .is_err()
        );
    }
    assert!(is_prediction_payload(&[1, 3, 0]) && !is_prediction_payload(&[9, 9]));
}

#[test]
fn invalid_authored_network_data_is_rejected_and_defaults_come_from_the_scene() {
    for (name, data) in [
        ("engine.NetworkReplication", json!({"priority": 99.0})),
        (
            "engine.NetworkReplication",
            json!({"relevance": "sometimes"}),
        ),
        ("engine.NetworkReplication", json!({"max_update_hz": 0.1})),
        ("engine.NetworkReplication", json!({"unknown": 1})),
        ("engine.NetworkHitboxes", json!({"layers": 0})),
        ("engine.NetworkHitboxes", json!({"hitboxes": []})),
        (
            "engine.NetworkHitboxes",
            json!({"hitboxes": [{"shape":"sphere","radius":-1.0}]}),
        ),
        ("engine.NetworkVoice", json!({"proximity_distance": -3.0})),
        ("engine.NetworkVoice", json!({"channel": 300})),
        ("engine.NetworkSession", json!({"mode": "udp"})),
        (
            "engine.NetworkSession",
            json!({"conditions": {"loss": 1.5}}),
        ),
        ("engine.NetworkSession", json!({"bytes_per_second": 0})),
        (
            "engine.NetworkSession",
            json!({"voice": {"frame_ms": 1000}}),
        ),
    ] {
        let dir = project(
            vec![scene_entity("e", "server", [0.0; 3], json!({name: data}))],
            None,
        );
        assert!(
            Project::load(dir.path().join("project.json")).is_err(),
            "{name} should be rejected"
        );
    }
    // A replication rule needs a replica; only one session component is allowed.
    let dir = project(
        vec![
            json!({"id":"e","name":"e","components":{"Transform":{"position":[0,0,0]},"custom":{
            "engine.NetworkReplication":{"version":1,"data":{}}}}}),
        ],
        None,
    );
    assert!(Project::load(dir.path().join("project.json")).is_err());
    let session = |id: &str| {
        json!({"id":id,"name":id,"components":{"custom":{
            "engine.NetworkSession":{"version":1,"data":{}}}}})
    };
    let dir = project(vec![session("a"), session("b")], None);
    assert!(Project::load(dir.path().join("project.json")).is_err());
    let dir = project(vec![session("a")], None);
    assert!(Project::load(dir.path().join("project.json")).is_ok());

    // Authored defaults fill NetworkSettings.
    let dir = project(
        vec![],
        Some(
            json!({"mode":"delta","conditions":{"loss":0.2,"latency_ms":40,"seed":3},
            "bytes_per_second":50000,"view_radius":12.5,"voice":{"enabled":true,"max_distance":9.0,
            "routing":"channel_proximity"}}),
        ),
    );
    let runtime = runtime(&dir);
    let settings = NetworkSettings::from_runtime(&runtime).unwrap();
    assert_eq!(settings.mode, NetworkMode::Delta);
    let conditions = settings.conditions.unwrap();
    assert_eq!(
        (conditions.loss, conditions.latency_ms, conditions.seed),
        (0.2, 40, 3)
    );
    assert_eq!(
        (settings.bytes_per_second, settings.view_radius),
        (50000, 12.5)
    );
    let voice = settings.voice.unwrap();
    assert_eq!(voice.routing, VoiceRoutingKind::ChannelProximity);
    assert_eq!(voice.max_distance, 9.0);
    // No component means native defaults.
    let plain = project(vec![], None);
    let defaults = NetworkSettings::from_runtime(&self::runtime(&plain)).unwrap();
    assert_eq!(defaults.mode, NetworkMode::Snapshot);
    assert!(defaults.conditions.is_none());
}

#[test]
fn endpoint_attachment_validates_settings_and_session_state() {
    let dir = project(server_entities(json!({})), None);
    let mut rt = runtime(&dir);
    let Quic {
        server: endpoint, ..
    } = quic(0);
    let bad = NetworkSettings {
        bytes_per_second: 0,
        ..NetworkSettings::default()
    };
    assert!(attach_endpoint_server(&mut rt, Box::new(endpoint), bad).is_err());
    let Quic {
        server: endpoint, ..
    } = quic(0);
    attach_endpoint_server(&mut rt, Box::new(endpoint), NetworkSettings::default()).unwrap();
    let Quic { server: other, .. } = quic(0);
    assert!(attach_endpoint_server(&mut rt, Box::new(other), NetworkSettings::default()).is_err());
    assert!(is_attached(rt.app().world()));
    step(&mut rt);
    detach(rt.app_mut().world_mut());
    assert!(!is_attached(rt.app().world()));
}
