use bevy_ecs::prelude::*;
use engine_network::{Client, ClientConfig, InputCommand, Server, ServerConfig};
use engine_project::{networking::*, *};
use serde_json::{Value, json};
use std::{
    fs, thread,
    time::{Duration, Instant},
};

fn fixture(authority: &str, with_callback: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut hero = json!({"id":"hero","name":"hero","components":{
        "Transform":{"position":[0,0,0]},
        "custom":{"engine.NetworkReplica":{"version":1,"data":{"authority":authority}}}
    }});
    if with_callback {
        hero["components"]["EventBindings"] =
            json!({"bindings":[{"event":"network.input","handler":"move"}]});
    }
    fs::write(dir.path().join("scene.json"), serde_json::to_vec(&json!({"schema":1,"scene_id":"main","entities":[hero,
        {"id":"local","name":"local","components":{"Transform":{"position":[99,0,0]},"Script":{"asset":"counter"}}}
    ]})).unwrap()).unwrap();
    fs::write(
        dir.path().join("next.scene.json"),
        serde_json::to_vec(&json!({"schema":1,"scene_id":"next","entities":[]})).unwrap(),
    )
    .unwrap();
    fs::write(dir.path().join("counter.rs"), "pub fn configure() {}\n").unwrap();
    fs::write(dir.path().join("project.json"),serde_json::to_vec(&json!({"schema":1,"name":"network-test","startup_scene":"scene.json","assets":[{"id":"counter","kind":"script","path":"counter.rs"},{"id":"next","kind":"scene","path":"next.scene.json"}]})).unwrap()).unwrap();
    dir
}
#[derive(Resource, Default)]
struct Updates(usize);
struct Counter;
impl NativeScript for Counter {
    fn on_update(&mut self, world: &mut World, _: Entity, _: f32) -> engine_project::Result<()> {
        world.resource_mut::<Updates>().0 += 1;
        Ok(())
    }
}
fn counter(_: &mut World, _: Entity) -> engine_project::Result<Box<dyn NativeScript>> {
    Ok(Box::new(Counter))
}
fn movement(world: &mut World, owner: Entity, event: &EngineEvent) -> engine_project::Result<()> {
    let value = event.payload["movement"][0].as_f64().unwrap() as f32;
    world.get_mut::<Transform>(owner).unwrap().position[0] += value;
    Ok(())
}
fn runtime(dir: &tempfile::TempDir, callback: bool) -> Runtime {
    let mut runtime =
        Runtime::new(Project::load(dir.path().join("project.json")).unwrap()).unwrap();
    runtime.set_user_data_root(dir.path().join("data")).unwrap();
    runtime
        .app_mut()
        .world_mut()
        .insert_resource(Updates::default());
    runtime.register_script("counter", counter).unwrap();
    if callback {
        register_event_handler(runtime.app_mut(), "move", movement).unwrap();
    }
    runtime
}
fn step(runtime: &mut Runtime) {
    runtime.tick(1.0 / 60.0, InputSnapshot::default()).unwrap();
}
fn until(mut test: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !test() {
        assert!(Instant::now() < deadline, "runtime loopback timed out");
        thread::sleep(Duration::from_millis(1));
    }
}
fn pair(callback: bool) -> (tempfile::TempDir, tempfile::TempDir, Runtime, Runtime) {
    let server_dir = fixture("server", callback);
    let client_dir = fixture("remote", false);
    let mut server_runtime = runtime(&server_dir, callback);
    let mut client_runtime = runtime(&client_dir, false);
    let server = Server::bind(ServerConfig::default()).unwrap();
    let client = Client::connect(ClientConfig::new(server.local_addr().unwrap())).unwrap();
    attach_server(&mut server_runtime, server, NetworkSettings::default()).unwrap();
    attach_client(&mut client_runtime, client, NetworkSettings::default()).unwrap();
    until(|| {
        step(&mut server_runtime);
        step(&mut client_runtime);
        status(client_runtime.app().world()).unwrap().connected
    });
    (server_dir, client_dir, server_runtime, client_runtime)
}
#[test]
fn authorized_input_callback_moves_server_then_client_and_preserves_local_scripts() {
    let (_server_dir, _client_dir, mut server, mut client) = pair(true);
    let peer = status(client.app().world()).unwrap().peer_id.unwrap();
    assign_owner(server.app_mut().world_mut(), "hero", peer).unwrap();
    send_input(
        client.app_mut().world_mut(),
        InputCommand {
            tick: 1,
            entity_id: "hero".into(),
            movement: [1.0, 0.0, 0.0],
            buttons: 0,
        },
    )
    .unwrap();
    let mut observed = false;
    until(|| {
        step(&mut client);
        step(&mut server);
        observed |= server
            .snapshot()
            .events
            .iter()
            .any(|e| e.name == "network.input");
        server.position("hero").unwrap()[0] == 1.0 && client.position("hero").unwrap()[0] == 1.0
    });
    assert!(observed);
    assert_eq!(client.position("local").unwrap()[0], 99.0);
    assert!(client.app().world().resource::<Updates>().0 > 0);
    assert_eq!(status(server.app().world()).unwrap().accepted_inputs, 1);
    detach(server.app_mut().world_mut());
    detach(client.app_mut().world_mut());
    assert!(!is_attached(client.app().world()));
}
#[test]
fn input_without_game_callback_cannot_change_authoritative_pose() {
    let (_server_dir, _client_dir, mut server, mut client) = pair(false);
    let peer = status(client.app().world()).unwrap().peer_id.unwrap();
    assign_owner(server.app_mut().world_mut(), "hero", peer).unwrap();
    send_input(
        client.app_mut().world_mut(),
        InputCommand {
            tick: 1,
            entity_id: "hero".into(),
            movement: [1.0, 0.0, 0.0],
            buttons: 0,
        },
    )
    .unwrap();
    until(|| {
        step(&mut client);
        step(&mut server);
        status(server.app().world()).unwrap().accepted_inputs == 1
    });
    assert_eq!(server.position("hero").unwrap(), [0.0; 3]);
    revoke_owner(server.app_mut().world_mut(), "hero").unwrap();
    send_input(
        client.app_mut().world_mut(),
        InputCommand {
            tick: 2,
            entity_id: "hero".into(),
            movement: [1.0, 0.0, 0.0],
            buttons: 0,
        },
    )
    .unwrap();
    until(|| {
        step(&mut client);
        step(&mut server);
        status(server.app().world()).unwrap().rejected_inputs == 1
    });
    detach(server.app_mut().world_mut());
    detach(client.app_mut().world_mut());
}
#[test]
fn full_snapshot_despawns_and_respawns_only_allowlisted_remote_templates() {
    let (_server_dir, _client_dir, mut server, mut client) = pair(false);
    let original = server.entity("hero").unwrap();
    server.app_mut().world_mut().despawn(original);
    until(|| {
        step(&mut server);
        step(&mut client);
        client.entity("hero").is_none()
    });
    assert!(client.entity("local").is_some());
    let hero = server
        .app_mut()
        .world_mut()
        .spawn((
            runtime::AuthoredId {
                id: "hero".into(),
                name: "hero".into(),
            },
            Transform {
                position: [7.0, 0.0, 0.0],
                ..Default::default()
            },
            NetworkReplica {
                authority: ReplicaAuthority::Server,
            },
        ))
        .id();
    server
        .app_mut()
        .world_mut()
        .resource_mut::<runtime::EntityLookup>()
        .0
        .insert("hero".into(), hero);
    let unknown = server
        .app_mut()
        .world_mut()
        .spawn((
            runtime::AuthoredId {
                id: "unknown".into(),
                name: "unknown".into(),
            },
            Transform::default(),
            NetworkReplica {
                authority: ReplicaAuthority::Server,
            },
        ))
        .id();
    server
        .app_mut()
        .world_mut()
        .resource_mut::<runtime::EntityLookup>()
        .0
        .insert("unknown".into(), unknown);
    until(|| {
        step(&mut server);
        step(&mut client);
        client.position("hero").is_some_and(|p| p[0] == 7.0)
    });
    assert!(client.entity("unknown").is_none());
    assert_eq!(client.position("local").unwrap()[0], 99.0);
    detach(server.app_mut().world_mut());
    detach(client.app_mut().world_mut());
}
#[test]
fn remote_script_conflict_rejects_before_attachment() {
    let dir = fixture("remote", false);
    let scene_path = dir.path().join("scene.json");
    let mut scene: Value = serde_json::from_slice(&fs::read(&scene_path).unwrap()).unwrap();
    scene["entities"][0]["components"]["Script"] = json!({"asset":"counter"});
    fs::write(&scene_path, serde_json::to_vec(&scene).unwrap()).unwrap();
    let mut runtime = runtime(&dir, false);
    let server = Server::bind(ServerConfig::default()).unwrap();
    let client = Client::connect(ClientConfig::new(server.local_addr().unwrap())).unwrap();
    assert!(attach_client(&mut runtime, client, NetworkSettings::default()).is_err());
    assert!(!is_attached(runtime.app().world()));
}
#[test]
fn loading_replica_metadata_does_not_create_network_session() {
    let dir = fixture("remote", false);
    let mut runtime = runtime(&dir, false);
    runtime.tick(1.0 / 60.0, InputSnapshot::default()).unwrap();
    assert!(status(runtime.app().world()).is_none());
}

#[test]
fn remote_pose_application_rejects_local_ownership_takeover() {
    let (_server_dir, _client_dir, mut server, mut client) = pair(false);
    let hero = client.entity("hero").unwrap();
    let local = client.entity("local").unwrap();
    client
        .app_mut()
        .world_mut()
        .entity_mut(hero)
        .insert(runtime::OwnedBy(local));
    assert!(client.tick(1.0 / 60.0, InputSnapshot::default()).is_err());
    assert!(client.entity("local").is_some());
    assert!(!client.running());
    assert!(!is_attached(client.app().world()));
    detach(server.app_mut().world_mut());
    detach(client.app_mut().world_mut());
}

#[test]
fn attached_sessions_reject_both_scene_request_entrypoints_until_detached() {
    let (_server_dir, _client_dir, mut server, mut client) = pair(false);
    let revision = server.scene_revision();
    assert!(
        server
            .request_scene("next", "", "")
            .unwrap_err()
            .to_string()
            .contains("network_attached")
    );
    assert!(
        engine_project::services::request_scene(
            server.app_mut().world_mut(),
            runtime::SceneRequest {
                asset: "next".into(),
                spawn: String::new(),
                traveller: String::new()
            }
        )
        .is_err()
    );
    assert!(
        server
            .app()
            .world()
            .resource::<runtime::PendingScene>()
            .0
            .is_none()
    );
    assert_eq!(server.scene_revision(), revision);
    detach(server.app_mut().world_mut());
    server.request_scene("next", "", "").unwrap();
    step(&mut server);
    assert_eq!(server.active_scene().id, "next");
    assert_eq!(server.scene_revision(), revision + 1);
    assert!(server.entity("hero").is_none());
    client.stop().unwrap();
}

#[test]
fn runtime_shutdown_closes_sockets_and_keeps_final_snapshot_status() {
    let (_server_dir, _client_dir, mut server, mut client) = pair(false);
    server.stop().unwrap();
    assert!(!is_attached(server.app().world()));
    assert!(!server.snapshot().network.unwrap().connected);
    until(|| {
        step(&mut client);
        !client.snapshot().network.unwrap().connected
    });
    client.stop().unwrap();
    assert!(!is_attached(client.app().world()));
    assert!(client.snapshot().network.unwrap().last_error.is_some());
}

#[test]
fn remote_script_authority_fails_before_native_startup_callbacks_without_a_session() {
    let dir = fixture("remote", false);
    let path = dir.path().join("scene.json");
    let mut scene: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    scene["entities"][0]["components"]["Script"] = json!({"asset":"counter"});
    fs::write(path, serde_json::to_vec(&scene).unwrap()).unwrap();
    let mut runtime = runtime(&dir, false);
    runtime
        .app_mut()
        .schedule_mut(engine_core::Stage::Startup)
        .add_systems(|mut updates: ResMut<Updates>| {
            updates.0 += 1;
        });
    assert!(runtime.tick(1.0 / 60.0, InputSnapshot::default()).is_err());
    assert_eq!(runtime.app().world().resource::<Updates>().0, 0);
}
