use crate::{
    bindings,
    progression::{PASSAGE, Progress, SavedPosition},
};
use engine_core::bevy_ecs::prelude::{Entity, World};
use engine_project::{
    runtime::{
        self, ControlLocks, GameState, InputSettings, Persistence, SceneRequest, SpriteFrame,
        TriggerEvents,
    },
    ui::{PanelOptions, Rect, UiEvent, UiModel},
    *,
};
use serde_json::{Value, json};

pub struct Controller {
    variant: &'static str,
    mode: &'static str,
    state: Option<Progress>,
    initialized: bool,
    current_room: String,
    panel: Option<String>,
    status: String,
    elapsed: f32,
    scene_time: f32,
    preferences: Value,
    last_ui: Option<UiEvent>,
    muted: bool,
    loop_voice: Option<engine_audio::VoiceHandle>,
}
impl Controller {
    pub fn new(variant: &'static str, mode: &'static str) -> Self {
        Self {
            variant,
            mode,
            state: None,
            initialized: false,
            current_room: "room:A".into(),
            panel: None,
            status: String::new(),
            elapsed: 0.0,
            scene_time: 0.0,
            preferences: json!({"textScale":1,"reducedMotion":false}),
            last_ui: None,
            muted: false,
            loop_voice: None,
        }
    }
    fn s(&self) -> &Progress {
        self.state.as_ref().expect("initialized progress")
    }
    fn s_mut(&mut self) -> &mut Progress {
        self.state.as_mut().expect("initialized progress")
    }
    fn publish(&self, world: &mut World) -> Result<()> {
        let value = serde_json::to_value(self.s())?;
        world.resource_mut::<GameState>().0 = value.as_object().expect("progress object").clone();
        Ok(())
    }
    fn event(&mut self, world: &mut World, event: &str) -> Result<bool> {
        let changed = self.s_mut().event(event)?;
        self.publish(world)?;
        if changed && matches!(event, "quest:reward" | "story:reveal") {
            self.cue(world, 0.5);
        }
        Ok(changed)
    }
    fn show(&mut self, world: &mut World, id: &str, text: String, choices: &[&str]) -> Result<()> {
        self.close(world);
        world.resource_mut::<UiModel>().open(PanelOptions {
            id: id.into(),
            font_asset: "asset:font".into(),
            text,
            rect: Rect {
                x: 12.0,
                y: 48.0,
                width: 296.0,
                height: 118.0,
            },
            scale: self.preferences["textScale"].as_f64().unwrap_or(1.0) as f32,
            line_height: 8.0,
            glyph_advance: 6.0,
            choices: choices.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        })?;
        self.panel = Some(id.into());
        Ok(())
    }
    fn close(&mut self, world: &mut World) {
        if let Some(id) = self.panel.take() {
            world.resource_mut::<UiModel>().close(&id);
        }
    }
    fn request_room(&mut self, world: &mut World, room: &str) -> Result<()> {
        runtime::request_scene(
            world,
            SceneRequest {
                asset: if room == "room:B" {
                    "asset:room-b"
                } else {
                    "asset:room-a"
                }
                .into(),
                spawn: "entry".into(),
                traveller: "actor:player".into(),
            },
        )?;
        self.close(world);
        self.s_mut().room = room.into();
        if !self.s().restore_pending {
            self.s_mut().flock = false;
        }
        self.publish(world)
    }
    fn save_rest(&mut self, world: &mut World, owner: Entity) -> Result<()> {
        let p = position(world, owner)?;
        self.s_mut().flock = true;
        self.s_mut().position = Some(SavedPosition { x: p[0], y: p[1] });
        self.publish(world)?;
        let data = world.resource::<GameState>().0.clone();
        let result = world
            .resource::<Persistence>()
            .0
            .save("desktop-slot", data, None);
        match result {
            Ok(()) => {
                self.status = "SAVE CONFIRMED. Progress survives restart.".into();
                self.cue(world, 0.4);
            }
            Err(error) => {
                self.status = format!("SAVE FAILED. Previous save retained. {error}");
                return Err(error);
            }
        }
        Ok(())
    }
    fn apply_saved_position(&mut self, world: &mut World, owner: Entity) -> Result<()> {
        let p = self
            .s()
            .position
            .ok_or_else(|| invalid("snapshot has no saved position"))?;
        let original = position(world, owner)?;
        set_position(world, owner, [p.x, p.y, 0.0])?;
        let safe = runtime::safe_position(world, "actor:player", 2.0, 0.25)?;
        if safe.is_none_or(|s| (s[0] - p.x).abs() > 0.0001 || (s[1] - p.y).abs() > 0.0001) {
            set_position(world, owner, original)?;
            return Err(invalid("saved position overlaps a solid collider"));
        }
        self.s_mut().restore_pending = false;
        self.publish(world)?;
        self.status = "LOAD CONFIRMED. Room, position, choice and progress restored.".into();
        Ok(())
    }
    fn restore(&mut self, world: &mut World, owner: Entity) -> Result<()> {
        let saved = world
            .resource::<Persistence>()
            .0
            .load("desktop-slot", None)?;
        let previous = self.s().clone();
        self.state = Some(Progress::restore(Value::Object(saved))?);
        self.s_mut().mode = "interactive".into();
        self.s_mut().restore_pending = true;
        self.publish(world)?;
        let result = if self.s().room != self.current_room {
            self.request_room(world, &self.s().room.clone())
        } else {
            self.apply_saved_position(world, owner)
        };
        if result.is_err() {
            self.state = Some(previous);
            self.publish(world)?;
        }
        result
    }
    fn start_scene(&mut self, world: &mut World) -> Result<()> {
        if self.s().scene == "injury" || self.s().events.contains("scene:healed") {
            return Ok(());
        }
        self.s_mut().scene = "injury".into();
        self.scene_time = 0.0;
        world
            .resource_mut::<ControlLocks>()
            .0
            .insert("authored-scene".into());
        if let Some(bird) = runtime::find_entity(world, "actor:scene-dove") {
            visible(world, bird, true);
            set_position(world, bird, [0.0, 3.0, 0.0])?;
        }
        self.show(world,"scene","DEVELOPER AUTHORED INJURY\nA staged descent is separate from ordinary defeat.\nBack cancels safely. Wait to complete once.".into(),&[])?;
        self.publish(world)
    }
    fn cancel_scene(&mut self, world: &mut World) -> Result<()> {
        self.close(world);
        world
            .resource_mut::<ControlLocks>()
            .0
            .remove("authored-scene");
        self.reset_camera(world);
        self.s_mut().scene = "idle".into();
        self.scene_time = 0.0;
        if let Some(bird) = runtime::find_entity(world, "actor:scene-dove") {
            visible(world, bird, false);
        }
        self.status = "SCENE CANCELLED. Controls restored; no healing awarded.".into();
        self.publish(world)
    }
    fn finish_scene(&mut self, world: &mut World) -> Result<()> {
        self.event(world, "scene:injury")?;
        if self.event(world, "scene:healed")? {
            self.s_mut().heal_completions += 1;
        }
        self.close(world);
        world
            .resource_mut::<ControlLocks>()
            .0
            .remove("authored-scene");
        self.reset_camera(world);
        if let Some(bird) = runtime::find_entity(world, "actor:scene-dove") {
            visible(world, bird, false);
        }
        self.status = "SCENE COMPLETED ONCE. Followers remain two.".into();
        self.publish(world)
    }
    fn reset_camera(&self, world: &mut World) {
        if let Some(entity) = runtime::find_entity(world, "camera")
            && let Some(mut camera) = world.get_mut::<Camera2D>(entity)
        {
            camera.follow = Some("actor:player".into());
        }
    }
    fn save_settings(&mut self, world: &mut World, rebound: bool) -> Result<()> {
        let mut map = bindings::defaults();
        if rebound {
            map.insert("interact".into(), vec!["Key:Q".into(), "Gamepad:A".into()]);
        }
        let settings = json!({"version":1,"bindings":map,"audio":{"master":0.8,"music":0.25,"effects":0.7,"dialogue":0.9},"reducedMotion":true,"textScale":1,"interactRebound":rebound});
        world
            .resource::<Persistence>()
            .0
            .save_settings(settings.as_object().unwrap().clone(), None)?;
        world.resource_mut::<InputSettings>().rebind(map, 0.2)?;
        self.preferences = settings;
        self.apply_audio_settings(world)?;
        self.status = "SETTINGS SAVED. Interact now Q; restart retains it.".into();
        Ok(())
    }
    fn consume_ui(&mut self, world: &mut World) -> Result<()> {
        loop {
            let event = world.resource_mut::<UiModel>().poll_event();
            let Some(event) = event else {
                break;
            };
            self.last_ui = Some(event.clone());
            if event.kind == "cancel" {
                if event.panel == "scene" {
                    self.cancel_scene(world)?;
                } else {
                    self.close(world);
                }
            } else if event.kind == "confirm" {
                match event.panel.as_str() {
                    "variant" => {
                        self.s_mut().variant = if event.selection == 0 {
                            "male"
                        } else {
                            "female"
                        }
                        .into();
                        self.publish(world)?;
                        self.close(world);
                    }
                    "quest" => {
                        if event.selection == 0 {
                            self.event(world, "quest:accept")?;
                        }
                        self.s_mut().accepted_choice = Some(event.selection as u32 + 1);
                        self.publish(world)?;
                        self.close(world);
                    }
                    "scene" => {}
                    _ => self.close(world),
                }
            }
        }
        Ok(())
    }
    fn initialize(&mut self, world: &mut World, owner: Entity) -> Result<bool> {
        self.current_room = if runtime::find_entity(world, "room:B").is_some() {
            "room:B"
        } else {
            "room:A"
        }
        .into();
        let shared = world.resource::<GameState>().0.clone();
        if self.mode == "restore-test" && shared.is_empty() {
            let saved = world
                .resource::<Persistence>()
                .0
                .load("desktop-slot", None)?;
            self.state = Some(Progress::restore(Value::Object(saved))?);
            check(self.s().variant == self.variant, "wrong saved branch")?;
            verify_progress(self.s())?;
            let cfg = world.resource::<Persistence>().0.load_settings(None)?;
            self.preferences = Value::Object(cfg);
            check(
                self.preferences["interactRebound"] == true
                    && self.preferences["bindings"]["interact"][0] == "Key:Q"
                    && self.preferences["audio"]["music"] == 0.25,
                "settings/rebind lost at restart",
            )?;
            check(
                self.s().room == "room:B"
                    && self.s().position == Some(SavedPosition { x: 3.25, y: -2.25 })
                    && self.s().accepted_choice == Some(2)
                    && self.s().heal_completions == 1,
                "saved checkpoint lost",
            )?;
            self.s_mut().mode = "restore-test".into();
            self.s_mut().phase = 0;
            self.s_mut().restore_pending = true;
            log(
                world,
                format!("DESKTOP2D_RESTART_RESTORE_PASS:{}", self.s().variant),
            );
        } else if shared.contains_key("version") {
            self.state = Some(Progress::restore(Value::Object(shared))?);
        } else {
            self.state = Some(Progress::new(self.variant)?);
            self.s_mut().mode = self.mode.into();
        }
        if !self.s().restore_pending {
            self.s_mut().room = self.current_room.clone();
        }
        self.publish(world)?;
        if let Ok(cfg) = world.resource::<Persistence>().0.load_settings(None) {
            self.preferences = Value::Object(cfg);
            if let Ok(map) = serde_json::from_value(self.preferences["bindings"].clone()) {
                world.resource_mut::<InputSettings>().rebind(map, 0.2)?;
            }
        }
        world.resource_mut::<UiModel>().close("hud");
        world.resource_mut::<UiModel>().open(PanelOptions {
            id: "hud".into(),
            font_asset: "asset:font".into(),
            text: "DESKTOP 2D LAB".into(),
            rect: Rect {
                x: 8.0,
                y: 6.0,
                width: 304.0,
                height: 38.0,
            },
            modal: false,
            line_height: 8.0,
            glyph_advance: 6.0,
            ..Default::default()
        })?;
        self.start_music(world);
        self.apply_audio_settings(world)?;
        self.initialized = true;
        if self.s().restore_pending {
            if self.s().room != self.current_room {
                self.request_room(world, &self.s().room.clone())?;
                return Ok(true);
            }
            self.apply_saved_position(world, owner)?;
        }
        Ok(false)
    }
    fn follow(&self, world: &mut World, owner: Entity, dt: f32) -> Result<()> {
        let p = position(world, owner)?;
        for index in 1..=2 {
            let id = format!("actor:dove{index}");
            if let Some(bird) = runtime::find_entity(world, &id) {
                let point = position(world, bird)?;
                let (tx, ty) = if self.s().flock {
                    (
                        -4.0 + (index - 1) as f32 * 1.1,
                        1.2 + (index - 1) as f32 * 0.45,
                    )
                } else {
                    (p[0] - index as f32 * 0.65, p[1] + 0.6)
                };
                let (dx, dy) = (tx - point[0], ty - point[1]);
                if dx * dx + dy * dy > 36.0 {
                    set_position(world, bird, [tx, ty, 0.0])?;
                    if let Some(safe) = runtime::safe_position(world, &id, 2.0, 0.25)? {
                        set_position(world, bird, [safe[0], safe[1], 0.0])?;
                    }
                } else {
                    runtime::move_entity(
                        world,
                        &id,
                        [dx * (dt * 4.0).min(1.0), dy * (dt * 4.0).min(1.0)],
                    )?;
                }
            }
        }
        Ok(())
    }
    fn interact(&mut self, world: &mut World, owner: Entity) -> Result<()> {
        let mut overlaps = world
            .resource::<gameplay2d::World>()
            .overlaps("actor:player")?;
        overlaps.sort();
        match overlaps
            .iter()
            .find(|id| id.starts_with("interact:"))
            .map(String::as_str)
        {
            Some("interact:book") => self.show(world, "journal", self.s().page(), &[])?,
            Some("interact:quest") => {
                if self.s().quests["sample"] == "completed" {
                    self.event(world, "quest:reward")?;
                    self.show(
                        world,
                        "dialogue",
                        "REWARD RECORDED\nOne stable developer passage. Sources are retained."
                            .into(),
                        &[],
                    )?;
                } else {
                    self.show(
                        world,
                        "quest",
                        "DEVELOPER QUEST\nAccept, visit the blue objective in room B, and return."
                            .into(),
                        &["Accept", "Later"],
                    )?;
                }
            }
            Some("interact:objective") => {
                self.event(world, "quest:objective")?;
                self.show(
                    world,
                    "dialogue",
                    "OBJECTIVE OBSERVED\nReturn to the orange quest marker.".into(),
                    &[],
                )?;
            }
            Some("interact:exit") => self.request_room(
                world,
                if self.current_room == "room:A" {
                    "room:B"
                } else {
                    "room:A"
                },
            )?,
            Some("interact:rest") => {
                if let Err(error) = self.save_rest(world, owner) {
                    log(world, error.to_string());
                }
                self.show(world, "dialogue", self.status.clone(), &[])?;
            }
            _ => self.status = "Walk onto a marker, then inspect.".into(),
        }
        Ok(())
    }
    fn interactive(&mut self, world: &mut World, owner: Entity, dt: f32) -> Result<()> {
        if runtime::gameplay_input(world).pressed("journal") {
            self.show(world, "journal", self.s().page(), &[])?;
        }
        if runtime::gameplay_input(world).pressed("choose_variant") {
            self.show(
                world,
                "variant",
                "DEVELOPER VARIANT\nBoth placeholders share the same required route.".into(),
                &["Male", "Female"],
            )?;
        }
        if runtime::gameplay_input(world).pressed("interact") {
            self.interact(world, owner)?;
        }
        if runtime::gameplay_input(world).pressed("rest")
            && world
                .resource::<gameplay2d::World>()
                .overlaps("actor:player")?
                .contains(&"interact:rest".into())
        {
            if let Err(error) = self.save_rest(world, owner) {
                log(world, error.to_string());
            }
            self.show(world, "dialogue", self.status.clone(), &[])?;
        }
        if runtime::gameplay_input(world).pressed("load") {
            if let Err(error) = self.restore(world, owner) {
                self.status = format!("LOAD FAILED: {error}");
            }
            return Ok(());
        }
        if runtime::gameplay_input(world).pressed("new_game") {
            let variant = self.s().variant.clone();
            self.state = Some(Progress::new(&variant)?);
            self.publish(world)?;
            self.request_room(world, "room:A")?;
        }
        if runtime::gameplay_input(world).pressed("remap")
            && let Err(error) = self.save_settings(world, true)
        {
            self.status = format!("SETTINGS FAILED: {error}");
        }
        if runtime::gameplay_input(world).pressed("mute") {
            self.muted = !self.muted;
            self.mute(world)?;
            self.status = if self.muted {
                "MUTED. Visible feedback remains."
            } else {
                "AUDIO RESTORED."
            }
            .into();
        }
        if runtime::gameplay_input(world).pressed("defeat_demo") {
            self.event(world, "encounter:defeat")?;
            self.status = "ORDINARY DEFEAT: retry state, no healing.".into();
        }
        if runtime::gameplay_input(world).pressed("scene_demo") {
            self.start_scene(world)?;
        }
        if runtime::gameplay_input(world).pressed("story_demo") {
            self.event(world, "story:reveal")?;
        }
        let input = runtime::gameplay_input(world);
        let mut d = [
            input.value("move_right") - input.value("move_left"),
            input.value("move_up") - input.value("move_down"),
        ];
        let length = d[0].hypot(d[1]);
        if length > 1.0 {
            d[0] /= length;
            d[1] /= length;
        }
        if d != [0.0; 2] {
            self.s_mut().flock = false;
            runtime::move_entity(world, "actor:player", [d[0] * 3.0 * dt, d[1] * 3.0 * dt])?;
            self.publish(world)?;
        }
        Ok(())
    }
    fn cue(&self, world: &mut World, gain: f32) {
        let result = (|| -> Result<()> {
            let audio = world
                .get_resource::<runtime::RuntimeAudio>()
                .ok_or_else(|| invalid("audio unavailable"))?;
            let clip = audio
                .clips
                .get("asset:cue")
                .cloned()
                .ok_or_else(|| invalid("cue missing"))?;
            audio
                .control
                .play(
                    clip,
                    engine_audio::PlayOptions {
                        gain,
                        owner: "example:desktop-cue".into(),
                        ..Default::default()
                    },
                )
                .map_err(|e| invalid(&e.to_string()))?;
            Ok(())
        })();
        if let Err(error) = result {
            log(world, format!("CUE UNAVAILABLE: {error}"));
        }
    }
    fn start_music(&mut self, world: &mut World) {
        let result = (|| -> Result<engine_audio::VoiceHandle> {
            let audio = world
                .get_resource::<runtime::RuntimeAudio>()
                .ok_or_else(|| invalid("audio unavailable"))?;
            let clip = audio
                .clips
                .get("asset:loop")
                .cloned()
                .ok_or_else(|| invalid("loop missing"))?;
            audio
                .control
                .play(
                    clip,
                    engine_audio::PlayOptions {
                        looped: true,
                        bus: engine_audio::Bus::Music,
                        gain: 0.18,
                        owner: "example:desktop-loop".into(),
                        ..Default::default()
                    },
                )
                .map_err(|e| invalid(&e.to_string()))
        })();
        match result {
            Ok(voice) => self.loop_voice = Some(voice),
            Err(error) => log(
                world,
                format!("MUSIC UNAVAILABLE; VISUAL WALKTHROUGH STILL WORKS: {error}"),
            ),
        }
    }
    fn stop_music(&mut self, world: &mut World) {
        if let Some(audio) = world.get_resource::<runtime::RuntimeAudio>() {
            if let Some(voice) = self.loop_voice.take() {
                audio.control.stop(voice);
            }
            audio.control.stop_owner("example:desktop-cue");
        }
    }
    fn apply_audio_settings(&self, world: &mut World) -> Result<()> {
        if let Some(audio) = world.get_resource::<runtime::RuntimeAudio>() {
            for (name, bus) in [
                ("master", engine_audio::Bus::Master),
                ("music", engine_audio::Bus::Music),
                ("effects", engine_audio::Bus::Effects),
                ("dialogue", engine_audio::Bus::Dialogue),
            ] {
                if let Some(value) = self.preferences["audio"][name].as_f64() {
                    audio
                        .control
                        .set_bus_gain(bus, value as f32)
                        .map_err(|e| invalid(&e.to_string()))?;
                }
            }
        }
        Ok(())
    }
    fn mute(&self, world: &mut World) -> Result<()> {
        if let Some(audio) = world.get_resource::<runtime::RuntimeAudio>() {
            audio
                .control
                .set_bus_gain(
                    engine_audio::Bus::Master,
                    if self.muted { 0.0 } else { 0.8 },
                )
                .map_err(|e| invalid(&e.to_string()))?;
        }
        Ok(())
    }
    fn test_step(&mut self, world: &mut World, owner: Entity) -> Result<()> {
        let phase = self.s().phase;
        if self.s().mode == "restore-test" {
            let p = position(world, owner)?;
            check(
                self.current_room == "room:B"
                    && self.s().room == "room:B"
                    && (p[0] - 3.25).abs() < 0.0001
                    && (p[1] + 2.25).abs() < 0.0001
                    && self.s().accepted_choice == Some(2)
                    && !self.s().restore_pending,
                "saved room/position/choice not restored",
            )?;
            if phase == 0 {
                check(!self.event(world, "quest:reward")?, "duplicate reward")?;
                check(!self.event(world, "story:reveal")?, "duplicate story")?;
                check(!self.event(world, "scene:healed")?, "duplicate healing")?;
                self.s_mut().phase = 1;
            } else {
                verify_progress(self.s())?;
                self.s_mut().mode = "complete".into();
                log(
                    world,
                    format!("DESKTOP2D_RESTART_PASS:{}", self.s().variant),
                );
            }
            self.publish(world)?;
            return Ok(());
        }
        if self.s().mode != "new-test" {
            return Ok(());
        }
        match phase {
            0 => {
                check(
                    !self.s().reveals.contains(PASSAGE)
                        && !self.s().page().contains("Original placeholder"),
                    "locked text leaked",
                )?;
                check(
                    runtime::find_entity(world, "actor:player") == Some(owner),
                    "duplicate player",
                )?;
            }
            1 => {
                let result = runtime::move_entity(world, "actor:player", [100.0, 0.0])?;
                check(
                    !result.contacts.is_empty() && result.position[0] < 8.0,
                    "wall did not stop sweep",
                )?;
                set_position(world, owner, [-2.0, 0.0, 0.0])?;
            }
            2 => check(
                world
                    .resource::<gameplay2d::World>()
                    .overlaps("actor:player")?
                    .contains(&"interact:book".into()),
                "stable trigger overlap missing",
            )?,
            3 => check(
                world.resource::<TriggerEvents>().0.iter().any(|t| {
                    (t.first == "actor:player" && t.second == "interact:book")
                        || (t.second == "actor:player" && t.first == "interact:book")
                }),
                "trigger event missing",
            )?,
            4 => self.show(world, "journal", self.s().page(), &[])?,
            5 => {
                let input = world.resource::<ActionFrame>();
                check(
                    !input.held("move_right") && input.value("move_right") == 0.0,
                    "modal leaked world action",
                )?;
            }
            6 => {
                check(
                    self.last_ui
                        .as_ref()
                        .is_some_and(|e| e.kind == "confirm" && e.panel == "journal"),
                    "inject ui_confirm:6",
                )?;
                check(
                    !world.resource::<ActionFrame>().pressed("interact"),
                    "dismissal leaked interact",
                )?;
            }
            7 => self.show(
                world,
                "quest",
                "DEVELOPER QUEST\nChoose a placeholder response.".into(),
                &["Accept", "Later"],
            )?,
            8 => check(
                self.last_ui
                    .as_ref()
                    .is_some_and(|e| e.kind == "selection" && e.selection == 1),
                "inject ui_down:8",
            )?,
            9 => check(self.s().accepted_choice == Some(2), "inject ui_confirm:9")?,
            10 => {
                check(!self.event(world, "quest:reward")?, "early reward")?;
                self.event(world, "quest:accept")?;
                check(!self.event(world, "quest:accept")?, "duplicate acceptance")?;
            }
            11 => {
                self.event(world, "quest:objective")?;
                check(
                    self.s().quests["sample"] == "completed",
                    "objective did not complete quest",
                )?;
            }
            12 => {
                self.save_rest(world, owner)?;
                let middle = Progress::restore(Value::Object(
                    world
                        .resource::<Persistence>()
                        .0
                        .load("desktop-slot", None)?,
                ))?;
                check(
                    middle.quests["sample"] == "completed" && !middle.reveals.contains(PASSAGE),
                    "midquest state lost",
                )?;
            }
            13 => {
                check(self.event(world, "quest:reward")?, "missing reward")?;
                check(!self.event(world, "quest:reward")?, "duplicate reward")?;
            }
            14 => {
                self.event(world, "story:reveal")?;
                verify_progress(self.s())?;
            }
            15 => {
                self.show(world, "journal", self.s().page(), &[])?;
                world.resource_mut::<UiModel>().scroll("journal", 32.0)?;
            }
            16 => check(
                self.last_ui
                    .as_ref()
                    .is_some_and(|e| e.kind == "cancel" && e.panel == "journal"),
                "inject ui_back:16",
            )?,
            17 => self.start_scene(world)?,
            18 => {
                check(
                    self.last_ui
                        .as_ref()
                        .is_some_and(|e| e.kind == "cancel" && e.panel == "scene"),
                    "inject ui_back:18",
                )?;
                check(
                    self.s().scene == "idle" && !self.s().events.contains("scene:healed"),
                    "cancellation awarded healing",
                )?;
            }
            19 => {
                self.event(world, "encounter:defeat")?;
                check(
                    self.s().scene == "retry" && !self.s().events.contains("scene:healed"),
                    "ordinary defeat healed",
                )?;
            }
            20 => {
                self.start_scene(world)?;
                check(self.s().scene == "injury", "scene did not start")?;
            }
            21 => {
                if self.scene_time < 3.0 {
                    check(
                        self.s().scene == "injury" && !self.s().events.contains("scene:healed"),
                        "healing before three seconds",
                    )?;
                    return Ok(());
                }
                verify_progress(self.s())?;
                check(
                    self.s().scene == "idle" && self.s().heal_completions == 1,
                    "timed healing missing/duplicated",
                )?;
            }
            22 => {
                self.save_settings(world, true)?;
                self.save_rest(world, owner)?;
            }
            23 => {
                self.s_mut().phase = 24;
                self.publish(world)?;
                self.request_room(world, "room:B")?;
                return Ok(());
            }
            24 => {
                check(
                    self.current_room == "room:B",
                    "room B transition not accepted",
                )?;
                verify_progress(self.s())?;
                check(
                    runtime::find_entity(world, "actor:dove1").is_some()
                        && runtime::find_entity(world, "actor:dove2").is_some(),
                    "followers lost",
                )?;
                check(
                    runtime::safe_position(world, "actor:player", 2.0, 0.25)?.is_some(),
                    "no safe position",
                )?;
            }
            25 => check(
                runtime::request_scene(
                    world,
                    SceneRequest {
                        asset: "asset:missing-room".into(),
                        spawn: "entry".into(),
                        traveller: "actor:player".into(),
                    },
                )
                .is_err(),
                "missing scene accepted",
            )?,
            26 => {
                check(
                    self.current_room == "room:B",
                    "failed transition changed room",
                )?;
                self.s_mut().phase = 27;
                self.publish(world)?;
                self.request_room(world, "room:A")?;
                return Ok(());
            }
            27 => {
                check(self.current_room == "room:A", "return to A failed")?;
                verify_progress(self.s())?;
            }
            28 => {
                self.s_mut().phase = 29;
                self.publish(world)?;
                self.request_room(world, "room:B")?;
                return Ok(());
            }
            29 => {
                check(
                    self.current_room == "room:B" && self.s().accepted_choice == Some(2),
                    "checkpoint room/choice lost",
                )?;
                set_position(world, owner, [3.25, -2.25, 0.0])?;
                check(
                    runtime::safe_position(world, "actor:player", 2.0, 0.25)?.is_some(),
                    "unsafe checkpoint",
                )?;
                self.save_rest(world, owner)?;
                let checkpoint = Progress::restore(Value::Object(
                    world
                        .resource::<Persistence>()
                        .0
                        .load("desktop-slot", None)?,
                ))?;
                check(
                    checkpoint.room == "room:B"
                        && checkpoint.position == Some(SavedPosition { x: 3.25, y: -2.25 })
                        && checkpoint.accepted_choice == Some(2),
                    "durable checkpoint lost",
                )?;
                self.s_mut().mode = "complete".into();
                log(
                    world,
                    format!("DESKTOP2D_WALKTHROUGH_PASS:{}", self.s().variant),
                );
            }
            _ => return Err(invalid("unknown walkthrough phase")),
        }
        self.s_mut().phase = phase + 1;
        self.publish(world)
    }
}
impl NativeScript for Controller {
    fn on_update(&mut self, world: &mut World, owner: Entity, dt: f32) -> Result<()> {
        let room = if runtime::find_entity(world, "room:B").is_some() {
            "room:B"
        } else {
            "room:A"
        };
        // A scene transition (including reloading the same room) clears the UI model
        // and the retained player's script instance must rebuild its panels and music.
        let hud_missing = !world
            .resource::<UiModel>()
            .panels()
            .iter()
            .any(|panel| panel.options.id == "hud");
        if self.initialized && (room != self.current_room || hud_missing) {
            self.stop_music(world);
            self.initialized = false;
            self.panel = None;
        }
        if !self.initialized && self.initialize(world, owner)? {
            return Ok(());
        }
        self.elapsed += dt;
        self.consume_ui(world)?;
        if matches!(self.s().mode.as_str(), "new-test" | "restore-test") {
            self.test_step(world, owner)?;
        } else if self.s().mode == "interactive" {
            self.interactive(world, owner, dt)?;
        }
        if self.s().scene == "injury" {
            self.scene_time += dt;
            if let Some(bird) = runtime::find_entity(world, "actor:scene-dove") {
                set_position(
                    world,
                    bird,
                    [
                        0.0,
                        if self.preferences["reducedMotion"] == true {
                            0.0
                        } else {
                            (3.0 - self.scene_time).max(0.0)
                        },
                        0.0,
                    ],
                )?;
            }
            if let Some(camera) = runtime::find_entity(world, "camera")
                && let Some(mut camera) = world.get_mut::<Camera2D>(camera)
            {
                camera.follow = None;
                camera.center = [0.0; 2];
            }
            if self.scene_time >= 3.0 {
                self.finish_scene(world)?;
            }
        }
        self.follow(world, owner, dt)?;
        if self.preferences["reducedMotion"] == true {
            for id in [
                "actor:player",
                "actor:dove1",
                "actor:dove2",
                "actor:scene-dove",
                "effect:glow",
            ] {
                if let Some(entity) = runtime::find_entity(world, id) {
                    world.entity_mut(entity).insert(SpriteFrame(0));
                }
            }
        } else {
            let input = runtime::gameplay_input(world);
            let moving = ["move_left", "move_right", "move_up", "move_down"]
                .iter()
                .any(|a| input.held(a));
            world
                .entity_mut(owner)
                .insert(SpriteFrame(if moving || self.s().mode == "new-test" {
                    (self.elapsed * 6.0) as usize % 4
                } else {
                    0
                }));
        }
        let text = format!(
            "DESKTOP 2D LAB | {} | {}\nWASD move E inspect J page R rest L load\nH scene B defeat F rebind M mute P variant\n{}",
            self.current_room,
            self.s().variant,
            self.status
        );
        world.resource_mut::<UiModel>().set_text("hud", text)?;
        Ok(())
    }
    fn on_destroy(&mut self, world: &mut World, _: Entity) -> Result<()> {
        self.close(world);
        world.resource_mut::<UiModel>().close("hud");
        world
            .resource_mut::<ControlLocks>()
            .0
            .remove("authored-scene");
        self.stop_music(world);
        Ok(())
    }
}
fn position(world: &World, entity: Entity) -> Result<[f32; 3]> {
    world
        .get::<Transform>(entity)
        .map(|t| t.position)
        .ok_or_else(|| invalid("missing Transform"))
}
fn set_position(world: &mut World, entity: Entity, p: [f32; 3]) -> Result<()> {
    world
        .get_mut::<Transform>(entity)
        .ok_or_else(|| invalid("missing Transform"))?
        .position = p;
    Ok(())
}
fn visible(world: &mut World, entity: Entity, value: bool) {
    if let Some(mut sprite) = world.get_mut::<SpriteRenderer>(entity) {
        sprite.visible = value;
    }
}
fn invalid(message: &str) -> Error {
    Error::new("example.assertion", message)
}
fn check(value: bool, message: &str) -> Result<()> {
    if value { Ok(()) } else { Err(invalid(message)) }
}
fn verify_progress(s: &Progress) -> Result<()> {
    check(
        s.quests["sample"] == "reward_recorded"
            && s.reveals.contains(PASSAGE)
            && s.reveals.len() == 1
            && s.followers == 2
            && s.provenance
                .get(PASSAGE)
                .is_some_and(|p| p.contains("quest:sample") && p.contains("story:sample")),
        "reward/provenance/follower lost",
    )
}
fn log(world: &mut World, message: String) {
    world.resource_mut::<runtime::RuntimeLog>().0.push(message);
}

pub struct PureTests;
impl NativeScript for PureTests {
    fn on_create(&mut self, world: &mut World, _: Entity) -> Result<()> {
        for variant in ["male", "female"] {
            let mut s = Progress::new(variant)?;
            check(
                !s.page().contains("Original placeholder"),
                "locked text leaked",
            )?;
            check(!s.event("quest:reward")?, "premature reward accepted")?;
            s.event("quest:objective")?;
            s.event("quest:accept")?;
            s.event("quest:reward")?;
            s.event("story:reveal")?;
            verify_progress(&s)?;
        }
        log(world, "DESKTOP2D_PURE_PASS".into());
        Ok(())
    }
}
