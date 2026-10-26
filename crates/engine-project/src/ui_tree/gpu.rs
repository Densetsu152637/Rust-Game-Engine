//! CPU side of the UI render features of `engine-ui-render`: the shared
//! [`UiGpuQueue`], the routing rule that decides which draws leave the 2D
//! sprite path, and the status each feature reports.
//!
//! The features (`ui.sdf_text`, `ui.world`, `ui.color_filter`) are
//! `PipelineFeature`s owned by the GPU crate; they cannot read the runtime
//! (fonts, layout, localisation, the text atlas) from the extracted render
//! world. This module is the one hand-off: draw extraction publishes a
//! [`UiGpuFrame`] into the queue each frame and the features read it when
//! they encode. Nothing here touches a GPU type.
//!
//! **Routing rule (no double drawing).** A draw is routed to a feature only
//! while that feature is *ready* (registered and prepared on a renderer's
//! device) and *enabled* for the active camera; routed content is not emitted
//! as 2D draws. When the feature is missing, not yet prepared, disabled
//! through `Camera3D.render_features`, or the backend is 2D-only or not wgpu,
//! the content stays on the 2D path (coverage atlas, `DrawSpace::World`
//! meshes) and [`UiRenderStatus`] says why. See `docs/ui-render.md`.
use super::*;
use crate::{
    accessibility::ColorFilterSettings,
    content::{Mesh, Texture},
    runtime::Draw,
};
use bevy_ecs::prelude::Resource;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

/// Render feature name of the screen-space SDF text pass (Overlay stage).
pub const FEATURE_SDF_TEXT: &str = "ui.sdf_text";
/// Render feature name of the depth-tested world UI pass (Transparent stage).
pub const FEATURE_WORLD: &str = "ui.world";
/// Render feature name of the colour-vision filter (Composite stage).
pub const FEATURE_COLOR_FILTER: &str = "ui.color_filter";

/// The three UI render features.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UiGpuFeature {
    /// SDF glyph quads of screen-space UI text.
    SdfText,
    /// `engine.UiWorldPanel` and `engine.WorldText` in `Camera3D` scenes.
    World,
    /// The colour-vision filter.
    ColorFilter,
}

impl UiGpuFeature {
    pub const ALL: [Self; 3] = [Self::SdfText, Self::World, Self::ColorFilter];

    /// Name of the `PipelineFeature` that implements it.
    pub const fn render_feature(self) -> &'static str {
        match self {
            Self::SdfText => FEATURE_SDF_TEXT,
            Self::World => FEATURE_WORLD,
            Self::ColorFilter => FEATURE_COLOR_FILTER,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::SdfText => 0,
            Self::World => 1,
            Self::ColorFilter => 2,
        }
    }

    fn unregistered_reason(self) -> String {
        let fallback = match self {
            Self::SdfText => {
                "SDF families render through a coverage atlas at the pixel size they are drawn at"
            }
            Self::World => {
                "world panels and world text draw as 2D sprite-pass meshes without depth testing"
            }
            Self::ColorFilter => "the colour-vision filter is not applied",
        };
        format!(
            "the `{}` render feature is not registered (call engine_ui_render::register); {fallback}",
            self.render_feature()
        )
    }

    fn unprepared_reason(self) -> String {
        format!(
            "the `{}` render feature is registered but no wgpu renderer has prepared it \
             (2D-only or non-wgpu backend, or no frame has been presented yet)",
            self.render_feature()
        )
    }
}

/// How world-space UI turns toward the viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldBillboard {
    /// The entity's own rotation.
    None,
    /// Face the camera position on every axis (`UiBillboard::Full`).
    Full,
    /// Rotate about the world Y axis only (`UiBillboard::Yaw`).
    Yaw,
    /// Align with the camera's right and up axes (`WorldText.billboard`).
    Screen,
}

/// What a [`WorldUiItem`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorldUiKind {
    Panel,
    Text,
}

/// One mesh of a world UI item.
#[derive(Clone, Debug)]
pub struct WorldUiDraw {
    /// Item-local geometry: x right, y up, world units, z 0.
    pub mesh: Arc<Mesh>,
    /// `None` is a solid-colour mesh.
    pub texture: Option<Arc<Texture>>,
    /// The texture is an SDF atlas page (distance in alpha).
    pub sdf: bool,
}

/// A world panel or world text, placed by an entity matrix and drawn by
/// `ui.world`. Items are sorted back to front per view by the feature.
#[derive(Clone, Debug)]
pub struct WorldUiItem {
    pub id: String,
    pub kind: WorldUiKind,
    pub layer: i32,
    /// The entity's world matrix, column major, before any billboarding.
    pub world_from_local: [[f32; 4]; 4],
    pub billboard: WorldBillboard,
    /// In draw order.
    pub draws: Vec<WorldUiDraw>,
}

/// Everything the UI features draw in one frame.
#[derive(Clone, Debug, Default)]
pub struct UiGpuFrame {
    /// Logical size of the 2D camera: the coordinate space of [`Self::sdf_text`].
    pub ui_size: [u32; 2],
    /// Screen UI text drawn from SDF pages (UI-space meshes in logical pixels,
    /// y down), in draw order (layer, then ID).
    pub sdf_text: Vec<Draw>,
    pub world: Vec<WorldUiItem>,
    pub color_filter: ColorFilterSettings,
}

/// Which content leaves the 2D path this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UiGpuRouting {
    /// Screen UI text of SDF families goes to `ui.sdf_text`.
    pub sdf_text: bool,
    /// World panels and world text go to `ui.world`.
    pub world_ui: bool,
}

/// Whether a feature draws on the GPU or its content falls back to 2D.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UiFeatureState {
    Gpu,
    Fallback,
}

/// Report of one feature, in the style of `UpscalerStatus`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UiFeatureStatus {
    /// The `PipelineFeature` name.
    pub name: &'static str,
    pub state: UiFeatureState,
    /// Why the content stays on the 2D path (or is not filtered), when
    /// `state` is `fallback`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Frames the feature encoded since it was prepared.
    pub frames: u64,
    /// Draw calls of the last encoded frame.
    pub last_draws: u32,
    /// Items (SDF runs, world panels and texts) of the last encoded frame.
    pub last_items: u32,
}

/// State of the three UI render features.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UiRenderStatus {
    pub features: Vec<UiFeatureStatus>,
}

impl UiRenderStatus {
    pub fn feature(&self, feature: UiGpuFeature) -> Option<&UiFeatureStatus> {
        self.features
            .iter()
            .find(|f| f.name == feature.render_feature())
    }
}

/// What a feature measured while encoding one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UiFeatureReport {
    pub draws: u32,
    pub items: u32,
}

#[derive(Clone, Debug, Default)]
struct FeatureSlot {
    registered: bool,
    ready: bool,
    frames: u64,
    last: UiFeatureReport,
}

#[derive(Debug, Default)]
struct Shared {
    slots: [FeatureSlot; 3],
    frame: Arc<UiGpuFrame>,
    /// The last routing decision: reasons a registered feature was not used.
    sdf_off: Option<String>,
    world_off: Option<String>,
    filter_off: Option<String>,
    had_camera3d: bool,
}

/// The hand-off between draw extraction and the UI render features. A cheap
/// clone shares the state; the runtime inserts one into its world and
/// `engine_ui_render::register` hands the same handle to the features.
#[derive(Clone, Debug, Default, Resource)]
pub struct UiGpuQueue(Arc<Mutex<Shared>>);

impl UiGpuQueue {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A feature object was created for this queue.
    pub fn mark_registered(&self, feature: UiGpuFeature) {
        self.lock().slots[feature.index()].registered = true;
    }

    /// The feature object was dropped (also clears readiness).
    pub fn mark_unregistered(&self, feature: UiGpuFeature) {
        let mut shared = self.lock();
        let slot = &mut shared.slots[feature.index()];
        *slot = FeatureSlot::default();
        if !shared.slots.iter().any(|s| s.registered) {
            // Release the textures and meshes of the last published frame.
            shared.frame = Arc::default();
        }
    }

    /// The feature was prepared on a device (`true`) or dropped (`false`).
    /// Draw extraction routes content to a feature only while it is ready.
    pub fn set_ready(&self, feature: UiGpuFeature, ready: bool) {
        let mut shared = self.lock();
        let slot = &mut shared.slots[feature.index()];
        slot.ready = ready;
        if !ready {
            slot.frames = 0;
            slot.last = UiFeatureReport::default();
        }
    }

    pub fn is_registered(&self, feature: UiGpuFeature) -> bool {
        self.lock().slots[feature.index()].registered
    }

    pub fn is_ready(&self, feature: UiGpuFeature) -> bool {
        self.lock().slots[feature.index()].ready
    }

    /// Whether any feature was registered (extraction publishes frames only then).
    pub fn any_registered(&self) -> bool {
        self.lock().slots.iter().any(|s| s.registered)
    }

    /// Decide what leaves the 2D path for a frame whose active camera has
    /// `camera3d_overrides` (`Camera3D.render_features`), `None` without a
    /// 3D camera, and remember why a registered feature is not used.
    pub fn route(&self, camera3d_overrides: Option<&BTreeMap<String, bool>>) -> UiGpuRouting {
        let mut shared = self.lock();
        let disabled = |feature: UiGpuFeature| {
            camera3d_overrides
                .and_then(|o| o.get(feature.render_feature()))
                .is_some_and(|enabled| !enabled)
        };
        let ready = |shared: &Shared, f: UiGpuFeature| shared.slots[f.index()].ready;
        let sdf_text = ready(&shared, UiGpuFeature::SdfText) && !disabled(UiGpuFeature::SdfText);
        let world_ui = ready(&shared, UiGpuFeature::World)
            && camera3d_overrides.is_some()
            && !disabled(UiGpuFeature::World);
        shared.sdf_off = (!sdf_text && ready(&shared, UiGpuFeature::SdfText))
            .then(|| "disabled for the active camera through Camera3D.render_features".to_owned());
        shared.world_off = if world_ui || !ready(&shared, UiGpuFeature::World) {
            None
        } else if camera3d_overrides.is_none() {
            Some(
                "no active Camera3D: world UI keeps the 2D camera path (DrawSpace::World meshes)"
                    .to_owned(),
            )
        } else {
            Some("disabled for the active camera through Camera3D.render_features".to_owned())
        };
        shared.had_camera3d = camera3d_overrides.is_some();
        shared.filter_off = (ready(&shared, UiGpuFeature::ColorFilter)
            && disabled(UiGpuFeature::ColorFilter))
        .then(|| "disabled for the active camera through Camera3D.render_features".to_owned());
        UiGpuRouting { sdf_text, world_ui }
    }

    /// Replace the frame the features draw.
    pub fn publish(&self, frame: UiGpuFrame) {
        self.lock().frame = Arc::new(frame);
    }

    /// The last published frame (empty before the first).
    pub fn frame(&self) -> Arc<UiGpuFrame> {
        self.lock().frame.clone()
    }

    /// A feature encoded a frame.
    pub fn report(&self, feature: UiGpuFeature, report: UiFeatureReport) {
        let mut shared = self.lock();
        let slot = &mut shared.slots[feature.index()];
        slot.frames += 1;
        slot.last = report;
    }

    /// State of one feature: on the GPU, or the precise reason it is not.
    pub fn state_of(&self, feature: UiGpuFeature) -> (UiFeatureState, Option<String>) {
        let shared = self.lock();
        let slot = &shared.slots[feature.index()];
        let reason = if !slot.registered {
            Some(feature.unregistered_reason())
        } else if !slot.ready {
            Some(feature.unprepared_reason())
        } else {
            match feature {
                UiGpuFeature::SdfText => shared.sdf_off.clone(),
                UiGpuFeature::World => shared.world_off.clone(),
                UiGpuFeature::ColorFilter => shared.filter_off.clone().or_else(|| {
                    (shared.frame.color_filter.is_active() && slot.frames == 0).then(|| {
                        "requested, but no final presentation has run the colour filter yet"
                            .to_owned()
                    })
                }),
            }
        };
        let state = if reason.is_some() {
            UiFeatureState::Fallback
        } else {
            UiFeatureState::Gpu
        };
        (state, reason)
    }

    /// Snapshot of all three features.
    pub fn status(&self) -> UiRenderStatus {
        let features = UiGpuFeature::ALL
            .into_iter()
            .map(|feature| {
                let (state, reason) = self.state_of(feature);
                let shared = self.lock();
                let slot = &shared.slots[feature.index()];
                UiFeatureStatus {
                    name: feature.render_feature(),
                    state,
                    reason,
                    frames: slot.frames,
                    last_draws: slot.last.draws,
                    last_items: slot.last.items,
                }
            })
            .collect();
        UiRenderStatus { features }
    }
}

/// Whether `draw` is screen-UI text drawn from an SDF atlas page. Only a text
/// backend with SDF output enabled produces these; the draw list that
/// contains them must be handed to `ui.sdf_text`, never to the sprite pass.
pub fn is_sdf_draw(draw: &Draw) -> bool {
    draw.id.starts_with("game-ui:")
        && draw.id.rsplit_once(':').is_some_and(|(_, tail)| {
            tail.len() == 3
                && tail.starts_with('s')
                && tail[1..].bytes().all(|b| b.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overrides(entries: &[(&str, bool)]) -> BTreeMap<String, bool> {
        entries.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
    }

    #[test]
    fn nothing_routes_until_a_feature_is_ready() {
        let queue = UiGpuQueue::default();
        assert_eq!(queue.route(Some(&overrides(&[]))), UiGpuRouting::default());
        queue.mark_registered(UiGpuFeature::SdfText);
        assert_eq!(queue.route(None), UiGpuRouting::default());
        queue.set_ready(UiGpuFeature::SdfText, true);
        let routing = queue.route(None);
        assert!(routing.sdf_text && !routing.world_ui);
    }

    #[test]
    fn world_ui_needs_a_3d_camera_and_respects_overrides() {
        let queue = UiGpuQueue::default();
        queue.mark_registered(UiGpuFeature::World);
        queue.set_ready(UiGpuFeature::World, true);
        assert!(!queue.route(None).world_ui, "2D frames keep the 2D path");
        let (state, reason) = queue.state_of(UiGpuFeature::World);
        assert_eq!(state, UiFeatureState::Fallback);
        assert!(reason.unwrap().contains("no active Camera3D"));
        assert!(queue.route(Some(&overrides(&[]))).world_ui);
        assert_eq!(queue.state_of(UiGpuFeature::World).0, UiFeatureState::Gpu);
        let off = overrides(&[(FEATURE_WORLD, false)]);
        assert!(!queue.route(Some(&off)).world_ui);
        assert!(
            queue
                .state_of(UiGpuFeature::World)
                .1
                .unwrap()
                .contains("render_features")
        );
        // An explicit `true` is the default.
        assert!(
            queue
                .route(Some(&overrides(&[(FEATURE_WORLD, true)])))
                .world_ui
        );
    }

    #[test]
    fn reasons_are_precise_per_state() {
        let queue = UiGpuQueue::default();
        let reason = |q: &UiGpuQueue| q.state_of(UiGpuFeature::SdfText).1.unwrap();
        assert!(reason(&queue).contains("not registered"));
        queue.mark_registered(UiGpuFeature::SdfText);
        assert!(reason(&queue).contains("no wgpu renderer has prepared it"));
        queue.set_ready(UiGpuFeature::SdfText, true);
        assert_eq!(
            queue.state_of(UiGpuFeature::SdfText),
            (UiFeatureState::Gpu, None)
        );
        queue.set_ready(UiGpuFeature::SdfText, false);
        assert_eq!(
            queue.state_of(UiGpuFeature::SdfText).0,
            UiFeatureState::Fallback
        );
    }

    #[test]
    fn reports_accumulate_and_reset_with_readiness() {
        let queue = UiGpuQueue::default();
        queue.mark_registered(UiGpuFeature::World);
        queue.set_ready(UiGpuFeature::World, true);
        queue.report(UiGpuFeature::World, UiFeatureReport { draws: 3, items: 2 });
        queue.report(UiGpuFeature::World, UiFeatureReport { draws: 5, items: 4 });
        let status = queue.status();
        let world = status.feature(UiGpuFeature::World).unwrap();
        assert_eq!(
            (world.frames, world.last_draws, world.last_items),
            (2, 5, 4)
        );
        queue.set_ready(UiGpuFeature::World, false);
        let status = queue.status();
        assert_eq!(status.feature(UiGpuFeature::World).unwrap().frames, 0);
        assert_eq!(status.features.len(), 3);
    }

    #[test]
    fn filter_status_flags_a_request_before_final_presentation() {
        let queue = UiGpuQueue::default();
        queue.mark_registered(UiGpuFeature::ColorFilter);
        queue.set_ready(UiGpuFeature::ColorFilter, true);
        assert_eq!(
            queue.state_of(UiGpuFeature::ColorFilter).0,
            UiFeatureState::Gpu
        );
        queue.publish(UiGpuFrame {
            color_filter: ColorFilterSettings {
                kind: crate::accessibility::ColorFilterKind::Protanopia,
                ..Default::default()
            },
            ..Default::default()
        });
        assert!(
            queue
                .state_of(UiGpuFeature::ColorFilter)
                .1
                .unwrap()
                .contains("no final presentation")
        );
        queue.report(UiGpuFeature::ColorFilter, UiFeatureReport::default());
        assert_eq!(
            queue.state_of(UiGpuFeature::ColorFilter).0,
            UiFeatureState::Gpu
        );
    }

    #[test]
    fn sdf_draw_ids_are_recognised() {
        let draw = |id: &str| Draw {
            id: id.into(),
            mesh: Arc::new(Mesh::default()),
            texture: None,
            layer: 0,
            space: crate::runtime::DrawSpace::Ui,
            clip: None,
            state: Default::default(),
            motion: None,
            offset: [0.0; 3],
        };
        assert!(is_sdf_draw(&draw("game-ui:6162!00001:s00")));
        assert!(!is_sdf_draw(&draw("game-ui:6162!00001:200")));
        assert!(!is_sdf_draw(&draw("game-ui:6162!00001:2~000")));
        assert!(
            !is_sdf_draw(&draw("sprite:a:s00")),
            "entity IDs cannot spoof it"
        );
        assert!(!is_sdf_draw(&draw("game-ui:6162!00001:sword")));
    }
}
