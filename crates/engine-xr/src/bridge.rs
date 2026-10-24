//! Connects an [`XrSession`] (or the reason there is none) to an
//! `engine-project` world: status telemetry, tracking poses, controller input
//! for `XR:` binding tokens, and haptic requests.
//!
//! Host loop sketch (see docs/xr.md):
//! ```text
//! let bridge = XrWorldBridge::install(world)?;      // once, after Runtime::new
//! loop {
//!     session.poll_events()?;
//!     let report = session.frame(&mut renderer, mirror)?;
//!     bridge.publish_session(world, &session);      // poses, input, status
//!     runtime.tick(dt)?;                            // gameplay reads XrTracking / actions
//!     bridge.apply_haptics(world, &session);        // drains request_haptic queue
//! }
//! ```
use crate::{
    XrError, XrSession,
    session::{ReferenceSpace, XrSessionConfig},
    status::XrStatus,
};
use bevy_ecs::prelude::World;
use engine_project::xr::{self as px, XrInputFeed, XrInputSource, XrStatusReport};

/// The engine-project status report of an [`XrStatus`].
pub fn status_report(status: &XrStatus) -> XrStatusReport {
    XrStatusReport::new(
        status.state(),
        status.to_string(),
        status.runtime().map(str::to_owned),
    )
}

/// Session options from authored settings (device features stay default).
pub fn session_config(settings: &px::XrSettings) -> XrSessionConfig {
    XrSessionConfig {
        reference_space: match settings.reference_space {
            px::XrReferenceSpace::Stage => ReferenceSpace::Stage,
            px::XrReferenceSpace::Local => ReferenceSpace::Local,
        },
        render_scale: settings.render_scale,
        near: settings.near_clip,
        far: settings.far_clip,
        ..XrSessionConfig::default()
    }
}

/// World-side half of an XR host.
pub struct XrWorldBridge {
    feed: XrInputFeed,
}
impl XrWorldBridge {
    /// Register the XR controller input source (deadzones from the active
    /// `engine.XrSettings`, else defaults) and an empty [`px::XrTracking`].
    pub fn install(world: &mut World) -> engine_project::Result<Self> {
        let settings = px::active_settings(world)?
            .map(|(_, s)| s)
            .unwrap_or_default();
        let feed = XrInputFeed::default();
        engine_project::input::register_input_source(
            world,
            XrInputSource::new(feed.clone(), &settings),
        )?;
        world.insert_resource(px::XrTracking::default());
        Ok(Self { feed })
    }
    pub fn feed(&self) -> &XrInputFeed {
        &self.feed
    }
    /// Publish a status without a session (discovery failures, no headset)
    /// and neutralise XR input.
    pub fn publish_status(&self, world: &mut World, status: &XrStatus) {
        self.feed.set(Default::default());
        px::publish_status(world, status_report(status));
    }
    /// Publish this frame's poses, controller input and session status.
    pub fn publish_session(&self, world: &mut World, session: &XrSession) {
        self.feed.set(session.controls());
        world.insert_resource(session.tracking().clone());
        px::publish_status(world, status_report(&session.status()));
    }
    /// Drain queued haptic requests into the session; returns failures.
    pub fn apply_haptics(&self, world: &mut World, session: &XrSession) -> Vec<XrError> {
        px::drain_haptics(world)
            .iter()
            .filter_map(|pulse| session.apply_haptic(pulse).err())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::SessionPhase;
    use engine_project::input::{ActionFrame, BindingMap, InputDevices, poll_devices};
    use engine_project::runtime::InputSettings;
    use openxr::sys;

    #[test]
    fn status_reports_carry_state_reason_and_runtime() {
        let report = status_report(&XrStatus::NoHeadset {
            runtime: "Oculus OpenXR 1.2.3".into(),
            result: sys::Result::ERROR_FORM_FACTOR_UNAVAILABLE,
        });
        assert_eq!(report.state, "no_headset");
        assert!(report.reason.contains("XR_ERROR_FORM_FACTOR_UNAVAILABLE"));
        assert_eq!(report.runtime.as_deref(), Some("Oculus OpenXR 1.2.3"));
        let running = status_report(&XrStatus::Session {
            runtime: "r".into(),
            phase: SessionPhase::Visible,
        });
        assert_eq!(running.state, "session_running");
    }

    #[test]
    fn authored_settings_become_session_options() {
        let settings = px::XrSettings {
            reference_space: px::XrReferenceSpace::Local,
            render_scale: 1.5,
            near_clip: 0.1,
            far_clip: 250.0,
            ..Default::default()
        };
        let config = session_config(&settings);
        assert_eq!(config.reference_space, ReferenceSpace::Local);
        assert_eq!(
            (config.render_scale, config.near, config.far),
            (1.5, 0.1, 250.0)
        );
    }

    #[test]
    fn xr_controller_state_drives_authored_actions() {
        let mut world = World::new();
        let bindings: BindingMap = [
            (
                "grab".to_owned(),
                vec!["XR:RightGrip".to_owned(), "Key:G".to_owned()],
            ),
            (
                "strafe_left".to_owned(),
                vec!["XR:LeftThumbstickXNegative".to_owned()],
            ),
            ("menu".to_owned(), vec!["XR:LeftMenu".to_owned()]),
        ]
        .into();
        world.insert_resource(InputSettings {
            bindings,
            deadzone: 0.2,
            revision: 1,
        });
        let bridge = XrWorldBridge::install(&mut world).unwrap();
        let mut state = px::XrControllerState {
            connected: true,
            ..Default::default()
        };
        // Controls already held when the device connects are quarantined
        // until released, so connect with neutral controls first.
        bridge.feed().set(state);
        assert!(
            poll_devices(&mut world, ActionFrame::default())
                .held
                .is_empty()
        );
        state.right.grip = 0.525; // deadzone 0.05 -> (0.525-0.05)/0.95 = 0.5
        state.left.thumbstick = [-0.575, 0.0]; // deadzone 0.15 -> 0.5
        bridge.feed().set(state);
        let frame = poll_devices(&mut world, ActionFrame::default());
        assert!((frame.value("grab") - 0.5).abs() < 1e-5, "{frame:?}");
        assert!((frame.value("strafe_left") - 0.5).abs() < 1e-5, "{frame:?}");
        assert!(frame.pressed("grab") && frame.held("strafe_left"));
        assert!(!frame.held("menu"));
        state.left.menu = true;
        state.right.grip = 0.0;
        bridge.feed().set(state);
        let frame = poll_devices(&mut world, ActionFrame::default());
        assert!(frame.pressed("menu"));
        assert!(frame.released("grab"));
        // Session loss neutralises XR input and reports the reason.
        bridge.publish_status(
            &mut world,
            &XrStatus::SessionLost {
                runtime: "r".into(),
                reason: "gone".into(),
            },
        );
        let frame = poll_devices(&mut world, ActionFrame::default());
        assert!(frame.held.is_empty(), "{frame:?}");
        let snapshot = world.resource::<InputDevices>().snapshot();
        assert!(
            snapshot
                .devices
                .iter()
                .any(|d| d.descriptor.id == "xr" && !d.connected)
        );
        assert_eq!(px::status(&world).unwrap().state, "session_lost");
    }
}
