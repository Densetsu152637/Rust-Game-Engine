//! A running OpenXR session: lifecycle, action input, tracking, haptics and
//! the per-frame wait/begin/render/end loop with per-eye swapchains.
use crate::{
    input::{self, ActionId, Hand},
    lifecycle::{ExitRequest, LifecycleAction, SessionLifecycle},
    render::{Eye, MirrorBlit, XrFrameContext, XrRenderTarget, XrViewRenderer},
    result_name,
    runtime::{XrInstance, XrSystem},
    status::XrStatus,
    view::{Pose, XrView, scaled_extent},
};
use engine_project::xr::{
    XrControllerState, XrHandControls, XrHapticPulse, XrHapticTarget, XrPose, XrTracking,
};
use openxr::sys;
use std::fmt;

/// Reference space for poses and the projection layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReferenceSpace {
    #[default]
    Stage,
    Local,
}

/// Session creation options.
#[derive(Clone, Debug)]
pub struct XrSessionConfig {
    pub reference_space: ReferenceSpace,
    /// Multiplier on the recommended per-eye resolution, in [0.25, 2].
    pub render_scale: f32,
    pub near: f32,
    pub far: f32,
    /// Features and limits the renderer needs on the XR device.
    pub features: wgpu::Features,
    pub limits: wgpu::Limits,
}
impl Default for XrSessionConfig {
    fn default() -> Self {
        Self {
            reference_space: ReferenceSpace::Stage,
            render_scale: 1.0,
            near: 0.05,
            far: 1000.0,
            features: wgpu::Features::empty(),
            limits: wgpu::Limits::default(),
        }
    }
}
impl XrSessionConfig {
    fn validate(&self) -> Result<(), XrError> {
        let scale_ok = self.render_scale.is_finite() && (0.25..=2.0).contains(&self.render_scale);
        let clip_ok = self.near.is_finite()
            && self.far.is_finite()
            && self.near > 0.0
            && self.far > self.near;
        if scale_ok && clip_ok {
            Ok(())
        } else {
            Err(XrError::InvalidInput(
                "render_scale must be in [0.25, 2] and 0 < near < far".into(),
            ))
        }
    }
}

/// Session and frame failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XrError {
    /// XR is not available; the status says exactly why.
    Unavailable(XrStatus),
    /// An OpenXR call failed.
    Runtime {
        operation: &'static str,
        result: sys::Result,
    },
    InvalidInput(String),
    /// The renderer or a GPU step failed.
    Render(String),
}
impl fmt::Display for XrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(status) => write!(f, "{status}"),
            Self::Runtime { operation, result } => {
                write!(
                    f,
                    "{operation} failed with {}: {result}",
                    result_name(*result)
                )
            }
            Self::InvalidInput(message) | Self::Render(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for XrError {}
fn runtime(operation: &'static str) -> impl Fn(sys::Result) -> XrError {
    move |result| XrError::Runtime { operation, result }
}

/// Outcome of one [`XrSession::frame`] call.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameReport {
    /// A frame was waited, begun and ended.
    pub submitted: bool,
    /// The renderer drew both eyes and a projection layer was submitted.
    pub rendered: bool,
    pub predicted_display_time_ns: i64,
    /// Left then right eye, when the views were located.
    pub views: Vec<XrView>,
}

/// Where the desktop mirror goes this frame.
pub struct MirrorTarget<'a> {
    pub eye: Eye,
    /// A view on a texture created from [`XrSession::device`].
    pub view: &'a wgpu::TextureView,
    pub format: wgpu::TextureFormat,
}

#[cfg(any(windows, target_os = "linux"))]
pub use live::XrSession;
#[cfg(not(any(windows, target_os = "linux")))]
pub use unsupported::XrSession;

#[cfg(not(any(windows, target_os = "linux")))]
mod unsupported {
    use super::*;
    /// Sessions need the Vulkan binding, built only for Windows and Linux.
    pub struct XrSession(std::convert::Infallible);
    impl XrSession {
        pub fn create(
            instance: XrInstance,
            _: XrSystem,
            _: XrSessionConfig,
        ) -> Result<Self, XrError> {
            Err(XrError::Unavailable(XrStatus::GraphicsBindingUnavailable {
                runtime: instance.runtime().to_owned(),
                reason: "the engine's Vulkan XR binding is built only for Windows and Linux".into(),
            }))
        }
        pub fn status(&self) -> XrStatus {
            match self.0 {}
        }
        pub fn tracking(&self) -> &XrTracking {
            match self.0 {}
        }
        pub fn controls(&self) -> XrControllerState {
            match self.0 {}
        }
        pub fn apply_haptic(&self, _: &XrHapticPulse) -> Result<(), XrError> {
            match self.0 {}
        }
    }
}

#[cfg(any(windows, target_os = "linux"))]
mod live {
    use super::*;
    use crate::vulkan::{self, GraphicsRequest, VulkanGraphics};
    use openxr as xr;
    use std::sync::{Arc, Mutex, MutexGuard};

    // HAL textures keep this owner alive through deferred GPU resource cleanup.
    // The callback runs before the Vulkan device that owns the texture dies.
    struct SwapchainOwner {
        handle: Mutex<xr::Swapchain<xr::Vulkan>>,
        // xr::Swapchain retains its session/instance, but not our library guard.
        _library: crate::negotiate::LibraryGuard,
    }
    impl SwapchainOwner {
        fn lock(&self) -> Result<MutexGuard<'_, xr::Swapchain<xr::Vulkan>>, XrError> {
            self.handle
                .lock()
                .map_err(|_| XrError::Render("XR swapchain lock poisoned".into()))
        }
    }

    // Publish only a complete stereo pair; failures drop all temporary owners.
    fn build_stereo<T, E>(mut build: impl FnMut(Eye) -> Result<T, E>) -> Result<[T; 2], E> {
        Ok([build(Eye::Left)?, build(Eye::Right)?])
    }

    fn clear_inactive_input(
        lifecycle: &SessionLifecycle,
        controls: &mut XrControllerState,
        tracking: &mut XrTracking,
    ) {
        if lifecycle.phase() != crate::SessionPhase::Focused || !lifecycle.frame_loop_active() {
            clear_input(controls, tracking);
        }
    }

    fn clear_input(controls: &mut XrControllerState, tracking: &mut XrTracking) {
        *controls = XrControllerState::default();
        *tracking = XrTracking {
            frame: tracking.frame,
            ..Default::default()
        };
    }

    // A failed image ownership step leaves acquire/wait/release progress
    // unknown. Recreate the session rather than retrying a half-acquired pair.
    fn guard_image_ownership<T>(
        result: Result<T, XrError>,
        lifecycle: &mut SessionLifecycle,
        controls: &mut XrControllerState,
        tracking: &mut XrTracking,
    ) -> Result<T, XrError> {
        if let Err(error) = &result {
            lifecycle.on_lost(format!(
                "swapchain image ownership failed: {error}; recreate the session"
            ));
            clear_input(controls, tracking);
        }
        result
    }

    struct Actions {
        trigger: xr::Action<f32>,
        grip: xr::Action<f32>,
        thumbstick: xr::Action<xr::Vector2f>,
        thumbstick_click: xr::Action<bool>,
        primary: xr::Action<bool>,
        secondary: xr::Action<bool>,
        menu: xr::Action<bool>,
        haptic: xr::Action<xr::Haptic>,
        grip_spaces: [xr::Space; 2],
        aim_spaces: [xr::Space; 2],
        // Pose actions stay alive as long as the spaces created from them.
        _grip_pose: xr::Action<xr::Posef>,
        _aim_pose: xr::Action<xr::Posef>,
        set: xr::ActionSet,
        hands: [xr::Path; 2],
    }
    impl Actions {
        fn new(
            instance: &xr::Instance,
            session: &xr::Session<xr::Vulkan>,
            notes: &mut Vec<String>,
        ) -> Result<Self, XrError> {
            let path = |p: &str| {
                instance
                    .string_to_path(p)
                    .map_err(runtime("xrStringToPath"))
            };
            let hands = [path(Hand::Left.path())?, path(Hand::Right.path())?];
            let set = instance
                .create_action_set(input::ACTION_SET, "Engine", 0)
                .map_err(runtime("xrCreateActionSet"))?;
            macro_rules! action {
                ($ty:ty, $id:expr) => {
                    set.create_action::<$ty>($id.name(), $id.localized_name(), &hands)
                        .map_err(runtime("xrCreateAction"))?
                };
            }
            let trigger = action!(f32, ActionId::Trigger);
            let grip = action!(f32, ActionId::Grip);
            let thumbstick = action!(xr::Vector2f, ActionId::Thumbstick);
            let thumbstick_click = action!(bool, ActionId::ThumbstickClick);
            let primary = action!(bool, ActionId::Primary);
            let secondary = action!(bool, ActionId::Secondary);
            let menu = action!(bool, ActionId::Menu);
            let grip_pose = action!(xr::Posef, ActionId::GripPose);
            let aim_pose = action!(xr::Posef, ActionId::AimPose);
            let haptic = action!(xr::Haptic, ActionId::Haptic);
            for profile in input::PROFILES {
                let suggested = input::suggested_bindings(profile).unwrap_or_default();
                let mut bindings = Vec::with_capacity(suggested.len());
                for binding in &suggested {
                    let target = path(&binding.path)?;
                    bindings.push(match binding.action {
                        ActionId::Trigger => xr::Binding::new(&trigger, target),
                        ActionId::Grip => xr::Binding::new(&grip, target),
                        ActionId::Thumbstick => xr::Binding::new(&thumbstick, target),
                        ActionId::ThumbstickClick => xr::Binding::new(&thumbstick_click, target),
                        ActionId::Primary => xr::Binding::new(&primary, target),
                        ActionId::Secondary => xr::Binding::new(&secondary, target),
                        ActionId::Menu => xr::Binding::new(&menu, target),
                        ActionId::GripPose => xr::Binding::new(&grip_pose, target),
                        ActionId::AimPose => xr::Binding::new(&aim_pose, target),
                        ActionId::Haptic => xr::Binding::new(&haptic, target),
                    });
                }
                if let Err(result) =
                    instance.suggest_interaction_profile_bindings(path(profile)?, &bindings)
                {
                    notes.push(format!(
                        "{profile} bindings rejected with {}",
                        result_name(result)
                    ));
                }
            }
            session
                .attach_action_sets(&[&set])
                .map_err(runtime("xrAttachSessionActionSets"))?;
            let space = |action: &xr::Action<xr::Posef>, hand: xr::Path| {
                action
                    .create_space(session, hand, xr::Posef::IDENTITY)
                    .map_err(runtime("xrCreateActionSpace"))
            };
            Ok(Self {
                grip_spaces: [space(&grip_pose, hands[0])?, space(&grip_pose, hands[1])?],
                aim_spaces: [space(&aim_pose, hands[0])?, space(&aim_pose, hands[1])?],
                trigger,
                grip,
                thumbstick,
                thumbstick_click,
                primary,
                secondary,
                menu,
                haptic,
                _grip_pose: grip_pose,
                _aim_pose: aim_pose,
                set,
                hands,
            })
        }
        fn controls(
            &self,
            session: &xr::Session<xr::Vulkan>,
            hand: usize,
        ) -> Result<XrHandControls, XrError> {
            let path = self.hands[hand];
            let float = |a: &xr::Action<f32>| {
                a.state(session, path)
                    .map(|s| {
                        if s.is_active {
                            s.current_state.clamp(0.0, 1.0)
                        } else {
                            0.0
                        }
                    })
                    .map_err(runtime("xrGetActionStateFloat"))
            };
            let button = |a: &xr::Action<bool>| {
                a.state(session, path)
                    .map(|s| s.is_active && s.current_state)
                    .map_err(runtime("xrGetActionStateBoolean"))
            };
            let stick = self
                .thumbstick
                .state(session, path)
                .map_err(runtime("xrGetActionStateVector2f"))?;
            let stick = if stick.is_active {
                [stick.current_state.x, stick.current_state.y].map(|v| {
                    if v.is_finite() {
                        v.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    }
                })
            } else {
                [0.0; 2]
            };
            Ok(XrHandControls {
                trigger: float(&self.trigger)?,
                grip: float(&self.grip)?,
                thumbstick: stick,
                thumbstick_click: button(&self.thumbstick_click)?,
                primary: button(&self.primary)?,
                secondary: button(&self.secondary)?,
                menu: button(&self.menu)?,
            })
        }
    }

    fn locate(space: &xr::Space, base: &xr::Space, time: xr::Time) -> Result<XrPose, XrError> {
        let location = space.locate(base, time).map_err(runtime("xrLocateSpace"))?;
        let valid =
            xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID;
        Ok(Pose::from_xr(location.pose).to_project(location.location_flags.contains(valid)))
    }

    struct EyeSwapchain {
        views: Vec<wgpu::TextureView>,
        textures: Vec<wgpu::Texture>,
        owner: Arc<SwapchainOwner>,
        // Public render targets use wgpu-owned memory, never runtime images.
        target: crate::render::EyeRenderTexture,
        extent: [u32; 2],
        format: wgpu::TextureFormat,
    }

    /// An OpenXR session rendering through a wgpu device. Field order is the
    /// teardown order: swapchains and spaces, then the session, then Vulkan,
    /// then the OpenXR instance and finally its library. Imported-image
    /// callbacks also retain runtime owners through deferred GPU cleanup.
    pub struct XrSession {
        swapchains: Vec<EyeSwapchain>,
        mirror: Option<MirrorBlit>,
        actions: Actions,
        head_space: xr::Space,
        base_space: xr::Space,
        frame_stream: xr::FrameStream<xr::Vulkan>,
        frame_waiter: xr::FrameWaiter,
        session: xr::Session<xr::Vulkan>,
        graphics: VulkanGraphics,
        system: XrSystem,
        config: XrSessionConfig,
        reference_space: ReferenceSpace,
        lifecycle: SessionLifecycle,
        events: xr::EventDataBuffer,
        tracking: XrTracking,
        controls: XrControllerState,
        notes: Vec<String>,
        instance: XrInstance,
    }

    impl XrSession {
        /// Create the Vulkan device through the runtime, the session, the
        /// engine action set and the reference spaces. The session starts in
        /// `idle`; call [`Self::poll_events`] every host frame.
        pub fn create(
            instance: XrInstance,
            system: XrSystem,
            config: XrSessionConfig,
        ) -> Result<Self, XrError> {
            config.validate()?;
            if system.eyes.len() != 2 {
                return Err(XrError::InvalidInput(
                    "a stereo session requires exactly two eyes".into(),
                ));
            }
            instance
                .check_graphics(&system)
                .map_err(XrError::Unavailable)?;
            let unavailable = |reason: String| {
                XrError::Unavailable(XrStatus::GraphicsBindingUnavailable {
                    runtime: instance.runtime().to_owned(),
                    reason,
                })
            };
            let request = GraphicsRequest {
                features: config.features,
                limits: config.limits.clone(),
            };
            let graphics = vulkan::create_graphics(&instance.instance, system.id, &request)
                .map_err(unavailable)?;
            let (session, frame_waiter, frame_stream) =
                vulkan::create_session(&instance.instance, system.id, &graphics)
                    .map_err(runtime("xrCreateSession"))?;
            let mut notes = Vec::new();
            let actions = Actions::new(&instance.instance, &session, &mut notes)?;
            let available = session
                .enumerate_reference_spaces()
                .map_err(runtime("xrEnumerateReferenceSpaces"))?;
            let reference_space = if config.reference_space == ReferenceSpace::Stage
                && !available.contains(&xr::ReferenceSpaceType::STAGE)
            {
                notes.push("STAGE reference space unavailable; using LOCAL".into());
                ReferenceSpace::Local
            } else {
                config.reference_space
            };
            let base_type = match reference_space {
                ReferenceSpace::Stage => xr::ReferenceSpaceType::STAGE,
                ReferenceSpace::Local => xr::ReferenceSpaceType::LOCAL,
            };
            let base_space = session
                .create_reference_space(base_type, xr::Posef::IDENTITY)
                .map_err(runtime("xrCreateReferenceSpace"))?;
            let head_space = session
                .create_reference_space(xr::ReferenceSpaceType::VIEW, xr::Posef::IDENTITY)
                .map_err(runtime("xrCreateReferenceSpace"))?;
            Ok(Self {
                swapchains: Vec::new(),
                mirror: None,
                actions,
                head_space,
                base_space,
                frame_stream,
                frame_waiter,
                session,
                graphics,
                system,
                config,
                reference_space,
                lifecycle: SessionLifecycle::default(),
                events: xr::EventDataBuffer::new(),
                tracking: XrTracking::default(),
                controls: XrControllerState::default(),
                notes,
                instance,
            })
        }
        /// The wgpu device every XR render resource must come from.
        pub fn device(&self) -> &wgpu::Device {
            &self.graphics.device
        }
        pub fn queue(&self) -> &wgpu::Queue {
            &self.graphics.queue
        }
        pub fn adapter(&self) -> &wgpu::Adapter {
            &self.graphics.adapter
        }
        pub fn wgpu_instance(&self) -> &wgpu::Instance {
            &self.graphics.instance
        }
        /// Vulkan API version of the runtime-created instance.
        pub fn vulkan_api_version(&self) -> u32 {
            self.graphics.api_version
        }
        pub fn system(&self) -> &XrSystem {
            &self.system
        }
        /// The reference space in use (STAGE falls back to LOCAL).
        pub fn reference_space(&self) -> ReferenceSpace {
            self.reference_space
        }
        /// Rejected interaction profiles and fallbacks, for diagnostics.
        pub fn notes(&self) -> &[String] {
            &self.notes
        }
        pub fn tracking(&self) -> &XrTracking {
            &self.tracking
        }
        pub fn controls(&self) -> XrControllerState {
            self.controls
        }
        pub fn lifecycle(&self) -> &SessionLifecycle {
            &self.lifecycle
        }
        /// The session must be dropped (exited or lost).
        pub fn is_finished(&self) -> bool {
            self.lifecycle.is_finished()
        }
        pub fn status(&self) -> XrStatus {
            let runtime = self.instance.runtime().to_owned();
            match self.lifecycle.lost_reason() {
                Some(reason) => XrStatus::SessionLost {
                    runtime,
                    reason: reason.to_owned(),
                },
                None => XrStatus::Session {
                    runtime,
                    phase: self.lifecycle.phase(),
                },
            }
        }

        /// Drain runtime events and perform lifecycle transitions.
        pub fn poll_events(&mut self) -> Result<(), XrError> {
            enum Kind {
                State(xr::SessionState),
                InstanceLoss,
                Other,
            }
            loop {
                let kind = match self.instance.instance.poll_event(&mut self.events) {
                    Ok(Some(xr::Event::SessionStateChanged(e))) => Kind::State(e.state()),
                    Ok(Some(xr::Event::InstanceLossPending(_))) => Kind::InstanceLoss,
                    Ok(Some(_)) => Kind::Other,
                    Ok(None) => return Ok(()),
                    Err(result) => return self.fail("xrPollEvent", result),
                };
                let action = match kind {
                    Kind::State(state) => self.lifecycle.on_state_changed(state),
                    Kind::InstanceLoss => self.lifecycle.on_instance_loss_pending(),
                    Kind::Other => LifecycleAction::None,
                };
                clear_inactive_input(&self.lifecycle, &mut self.controls, &mut self.tracking);
                match action {
                    LifecycleAction::BeginSession => match self
                        .session
                        .begin(xr::ViewConfigurationType::PRIMARY_STEREO)
                    {
                        Ok(_) => self.lifecycle.began(),
                        Err(result) => return self.fail("xrBeginSession", result),
                    },
                    LifecycleAction::EndSession => match self.session.end() {
                        Ok(_) => self.lifecycle.ended(),
                        Err(result) => return self.fail("xrEndSession", result),
                    },
                    LifecycleAction::DestroySession
                    | LifecycleAction::SessionLost
                    | LifecycleAction::None => {}
                }
            }
        }
        /// Record loss for loss results; always return the error.
        fn fail<T>(&mut self, operation: &'static str, result: sys::Result) -> Result<T, XrError> {
            if matches!(
                result,
                sys::Result::ERROR_SESSION_LOST | sys::Result::ERROR_INSTANCE_LOST
            ) {
                self.lifecycle
                    .on_lost(format!("{operation} returned {}", result_name(result)));
            }
            clear_input(&mut self.controls, &mut self.tracking);
            Err(XrError::Runtime { operation, result })
        }
        /// Ask the runtime to end the session; keep calling
        /// [`Self::poll_events`] until [`Self::is_finished`].
        pub fn request_exit(&mut self) -> Result<(), XrError> {
            let action = self.lifecycle.request_exit();
            clear_inactive_input(&self.lifecycle, &mut self.controls, &mut self.tracking);
            match action {
                ExitRequest::DestroyNow => Ok(()),
                ExitRequest::RequestExitSession => match self.session.request_exit() {
                    Ok(()) | Err(sys::Result::ERROR_SESSION_NOT_RUNNING) => Ok(()),
                    Err(result) => self.fail("xrRequestExitSession", result),
                },
            }
        }
        /// Vibrate one controller.
        pub fn apply_haptic(&self, pulse: &XrHapticPulse) -> Result<(), XrError> {
            let hand = match pulse.hand {
                XrHapticTarget::Left => 0,
                XrHapticTarget::Right => 1,
                XrHapticTarget::Both => {
                    return Err(XrError::InvalidInput(
                        "resolve Both into one pulse per hand".into(),
                    ));
                }
            };
            let amplitude = pulse.amplitude.clamp(0.0, 1.0);
            let duration = if pulse.duration_ms > 0.0 {
                xr::Duration::from_nanos((f64::from(pulse.duration_ms) * 1.0e6) as i64)
            } else {
                xr::Duration::MIN_HAPTIC
            };
            let vibration = xr::HapticVibration::new()
                .amplitude(amplitude)
                .duration(duration)
                .frequency(pulse.frequency_hz.max(0.0));
            self.actions
                .haptic
                .apply_feedback(&self.session, self.actions.hands[hand], &vibration)
                .map_err(runtime("xrApplyHapticFeedback"))
        }

        fn ensure_swapchains(&mut self) -> Result<(), XrError> {
            if !self.swapchains.is_empty() {
                return Ok(());
            }
            let formats = self
                .session
                .enumerate_swapchain_formats()
                .map_err(runtime("xrEnumerateSwapchainFormats"))?;
            let (vk_format, format) =
                crate::render::choose_swapchain_format(&formats).ok_or_else(|| {
                    XrError::Unavailable(XrStatus::GraphicsBindingUnavailable {
                        runtime: self.instance.runtime().to_owned(),
                        reason: format!(
                            "no 8-bit RGBA/BGRA swapchain format among the runtime's {formats:?}"
                        ),
                    })
                })?;
            let swapchains = build_stereo(|eye| -> Result<EyeSwapchain, XrError> {
                let index = eye.index();
                let eye = &self.system.eyes[index];
                let extent = scaled_extent(eye.recommended, eye.max, self.config.render_scale);
                let handle = self
                    .session
                    .create_swapchain(&xr::SwapchainCreateInfo {
                        create_flags: xr::SwapchainCreateFlags::EMPTY,
                        usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT
                            | xr::SwapchainUsageFlags::SAMPLED
                            | xr::SwapchainUsageFlags::TRANSFER_SRC,
                        format: vk_format,
                        sample_count: 1,
                        width: extent[0],
                        height: extent[1],
                        face_count: 1,
                        array_size: 1,
                        mip_count: 1,
                    })
                    .map_err(runtime("xrCreateSwapchain"))?;
                let images = handle
                    .enumerate_images()
                    .map_err(runtime("xrEnumerateSwapchainImages"))?;
                let label = if index == 0 {
                    "xr left eye"
                } else {
                    "xr right eye"
                };
                let owner = Arc::new(SwapchainOwner {
                    handle: Mutex::new(handle),
                    _library: self.instance._library.clone(),
                });
                let textures = images
                    .into_iter()
                    .map(|image| {
                        vulkan::wrap_swapchain_image(
                            &self.graphics.device,
                            image,
                            format,
                            extent,
                            label,
                            owner.clone(),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(XrError::Render)?;
                let views = textures
                    .iter()
                    .map(|t| t.create_view(&Default::default()))
                    .collect();
                let target = crate::render::EyeRenderTexture::new(
                    &self.graphics.device,
                    extent,
                    format,
                    label,
                );
                Ok(EyeSwapchain {
                    views,
                    textures,
                    owner,
                    target,
                    extent,
                    format,
                })
            })?;
            self.swapchains = Vec::from(swapchains);
            Ok(())
        }

        fn sample_input(&mut self, time: xr::Time) -> Result<(), XrError> {
            // Never publish a previous frame if syncing or any pose query fails.
            clear_input(&mut self.controls, &mut self.tracking);
            if self.lifecycle.phase() != crate::SessionPhase::Focused
                || !self.lifecycle.frame_loop_active()
            {
                return Ok(());
            }
            if let Err(result) = self
                .session
                .sync_actions(&[xr::ActiveActionSet::new(&self.actions.set)])
            {
                return self.fail("xrSyncActions", result);
            }
            let left = self.actions.controls(&self.session, 0)?;
            let right = self.actions.controls(&self.session, 1)?;
            let controls = XrControllerState {
                connected: true,
                left,
                right,
            };
            let base = &self.base_space;
            let mut tracking = XrTracking {
                head: locate(&self.head_space, base, time)?,
                predicted_display_time_ns: time.as_nanos(),
                frame: self.tracking.frame + 1,
                ..Default::default()
            };
            for hand in 0..2 {
                let pose = engine_project::xr::XrHandPose {
                    grip: locate(&self.actions.grip_spaces[hand], base, time)?,
                    aim: locate(&self.actions.aim_spaces[hand], base, time)?,
                };
                if hand == 0 {
                    tracking.left = pose;
                } else {
                    tracking.right = pose;
                }
            }
            self.controls = controls;
            self.tracking = tracking;
            Ok(())
        }

        /// Run one XR frame: wait, begin, sample input and poses, render both
        /// eyes through `renderer`, optionally mirror one eye, release and end.
        /// Returns without waiting when the session is not running.
        pub fn frame(
            &mut self,
            renderer: &mut dyn XrViewRenderer,
            mirror: Option<MirrorTarget<'_>>,
        ) -> Result<FrameReport, XrError> {
            if !self.lifecycle.frame_loop_active() {
                return Ok(FrameReport::default());
            }
            let state = match self.frame_waiter.wait() {
                Ok(state) => state,
                Err(result) => return self.fail("xrWaitFrame", result),
            };
            if let Err(result) = self.frame_stream.begin() {
                return self.fail("xrBeginFrame", result);
            }
            let time = state.predicted_display_time;
            let mut report = FrameReport {
                submitted: true,
                rendered: false,
                predicted_display_time_ns: time.as_nanos(),
                views: Vec::new(),
            };
            // Input first so gameplay and the renderer see this frame's poses.
            let input = self.sample_input(time);
            let mut ended = false;
            let rendered = if state.should_render && input.is_ok() {
                self.render(time, renderer, mirror, &mut report, &mut ended)
            } else {
                Ok(())
            };
            if !ended && let Err(result) = self.frame_stream.end(time, self.system.blend_mode, &[])
            {
                return self.fail("xrEndFrame", result);
            }
            for result in [rendered, input] {
                if let Err(error) = result {
                    return match error {
                        XrError::Runtime { operation, result } => self.fail(operation, result),
                        other => Err(other),
                    };
                }
            }
            report.rendered = ended && !report.views.is_empty();
            Ok(report)
        }

        /// Render and end the frame with a projection layer. Sets `ended` once
        /// `xrEndFrame` was called; otherwise the caller ends the frame empty.
        fn render(
            &mut self,
            time: xr::Time,
            renderer: &mut dyn XrViewRenderer,
            mirror: Option<MirrorTarget<'_>>,
            report: &mut FrameReport,
            ended: &mut bool,
        ) -> Result<(), XrError> {
            self.ensure_swapchains()?;
            let (flags, located) = self
                .session
                .locate_views(
                    xr::ViewConfigurationType::PRIMARY_STEREO,
                    time,
                    &self.base_space,
                )
                .map_err(runtime("xrLocateViews"))?;
            if !flags.contains(xr::ViewStateFlags::ORIENTATION_VALID) || located.len() != 2 {
                return Ok(());
            }
            let mut views = Vec::with_capacity(2);
            for (eye, (view, swapchain)) in Eye::BOTH
                .into_iter()
                .zip(located.iter().zip(&self.swapchains))
            {
                views.push(
                    XrView::new(
                        eye,
                        Pose::from_xr(view.pose),
                        view.fov.into(),
                        self.config.near,
                        self.config.far,
                        swapchain.extent,
                    )
                    .map_err(XrError::Render)?,
                );
            }
            let mut images = [0usize; 2];
            let acquired = (|| {
                for (index, swapchain) in self.swapchains.iter_mut().enumerate() {
                    let mut handle = swapchain.owner.lock()?;
                    images[index] = handle
                        .acquire_image()
                        .map_err(runtime("xrAcquireSwapchainImage"))?
                        as usize;
                    handle
                        .wait_image(xr::Duration::INFINITE)
                        .map_err(runtime("xrWaitSwapchainImage"))?;
                }
                Ok(())
            })();
            guard_image_ownership(
                acquired,
                &mut self.lifecycle,
                &mut self.controls,
                &mut self.tracking,
            )?;
            let targets: Vec<XrRenderTarget<'_>> = views
                .iter()
                .zip(&self.swapchains)
                .map(|(view, swapchain)| XrRenderTarget {
                    view,
                    texture: &swapchain.target.texture,
                    color: &swapchain.target.color,
                    format: swapchain.format,
                })
                .collect();
            let device = &self.graphics.device;
            let queue = &self.graphics.queue;
            let rendered = renderer.render_views(&XrFrameContext {
                device,
                queue,
                predicted_display_time_ns: time.as_nanos(),
                targets: &targets,
            });
            if rendered.is_ok() {
                let blit = self.mirror.get_or_insert_with(|| MirrorBlit::new(device));
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("xr swapchain and mirror blit"),
                });
                // Only internal commands can reference acquired runtime images.
                // Public renderer resources remain ordinary, independently owned textures.
                for ((target, swapchain), image) in targets.iter().zip(&self.swapchains).zip(images)
                {
                    blit.encode(
                        device,
                        &mut encoder,
                        target.color,
                        &swapchain.views[image],
                        swapchain.format,
                    );
                }
                if let Some(mirror) = &mirror {
                    blit.encode(
                        device,
                        &mut encoder,
                        targets[mirror.eye.index()].color,
                        mirror.view,
                        mirror.format,
                    );
                }
                queue.submit([encoder.finish()]);
            }
            let colors: Vec<&wgpu::TextureView> = self
                .swapchains
                .iter()
                .zip(images)
                .map(|(swapchain, image)| &swapchain.views[image])
                .collect();
            crate::render::finish_eye_layouts(device, queue, &colors);
            drop(targets);
            let released = (|| {
                for swapchain in &mut self.swapchains {
                    swapchain
                        .owner
                        .lock()?
                        .release_image()
                        .map_err(runtime("xrReleaseSwapchainImage"))?;
                }
                Ok(())
            })();
            guard_image_ownership(
                released,
                &mut self.lifecycle,
                &mut self.controls,
                &mut self.tracking,
            )?;
            rendered.map_err(XrError::Render)?;
            let handles = self
                .swapchains
                .iter()
                .map(|swapchain| swapchain.owner.lock())
                .collect::<Result<Vec<_>, _>>();
            let handles = guard_image_ownership(
                handles,
                &mut self.lifecycle,
                &mut self.controls,
                &mut self.tracking,
            )?;
            let projection_views: Vec<_> = located
                .iter()
                .zip(&self.swapchains)
                .zip(&handles)
                .map(|((view, swapchain), handle)| {
                    xr::CompositionLayerProjectionView::new()
                        .pose(view.pose)
                        .fov(view.fov)
                        .sub_image(
                            xr::SwapchainSubImage::new()
                                .swapchain(handle)
                                .image_array_index(0)
                                .image_rect(xr::Rect2Di {
                                    offset: xr::Offset2Di { x: 0, y: 0 },
                                    extent: xr::Extent2Di {
                                        width: swapchain.extent[0] as i32,
                                        height: swapchain.extent[1] as i32,
                                    },
                                }),
                        )
                })
                .collect();
            let layer = xr::CompositionLayerProjection::new()
                .space(&self.base_space)
                .views(&projection_views);
            *ended = true;
            let end = self
                .frame_stream
                .end(time, self.system.blend_mode, &[&layer]);
            drop(handles);
            if let Err(result) = end {
                return self.fail("xrEndFrame", result);
            }
            report.views = views;
            Ok(())
        }
    }

    impl Drop for XrSession {
        fn drop(&mut self) {
            // No runtime image is exposed to callers. Wait for our internal
            // submissions, destroy imports, then collect deferred resources.
            // Each HAL callback also retains the swapchain/session/library
            // until wgpu actually destroys the imported image wrapper.
            let _ = self.graphics.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            });
            for swapchain in &self.swapchains {
                for texture in &swapchain.textures {
                    texture.destroy();
                }
            }
            self.swapchains.clear();
            let _ = self.graphics.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            });
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[test]
        fn right_eye_failure_drops_left_eye_and_retry_builds_a_complete_pair() {
            struct EyeResource(Arc<AtomicUsize>);
            impl Drop for EyeResource {
                fn drop(&mut self) {
                    self.0.fetch_add(1, Ordering::SeqCst);
                }
            }
            let dropped = Arc::new(AtomicUsize::new(0));
            let mut published = Vec::new();
            for attempt in 0..2 {
                let built = build_stereo(|eye| {
                    if attempt == 0 && eye == Eye::Right {
                        return Err("right-eye import failed");
                    }
                    Ok(EyeResource(dropped.clone()))
                });
                match built {
                    Ok(pair) => published = Vec::from(pair),
                    Err(_) => {
                        assert!(published.is_empty());
                        assert_eq!(dropped.load(Ordering::SeqCst), 1);
                    }
                }
            }
            assert_eq!(published.len(), 2);
            // A right-eye mirror can now always index the complete pair.
            let _right = &published[Eye::Right.index()];
            drop(published);
            assert_eq!(dropped.load(Ordering::SeqCst), 3);
        }

        fn active_snapshot() -> (XrControllerState, XrTracking) {
            let pose = XrPose {
                tracked: true,
                position: [1.0; 3],
                ..Default::default()
            };
            (
                XrControllerState {
                    connected: true,
                    left: XrHandControls {
                        trigger: 1.0,
                        menu: true,
                        ..Default::default()
                    },
                    right: XrHandControls {
                        grip: 1.0,
                        thumbstick: [0.5; 2],
                        ..Default::default()
                    },
                },
                XrTracking {
                    head: pose,
                    left: engine_project::xr::XrHandPose {
                        grip: pose,
                        aim: pose,
                    },
                    right: engine_project::xr::XrHandPose {
                        grip: pose,
                        aim: pose,
                    },
                    predicted_display_time_ns: 123,
                    frame: 7,
                },
            )
        }

        #[test]
        fn focus_loss_stop_exit_and_loss_invalidate_controls_and_all_poses_without_a_frame() {
            for next in [
                xr::SessionState::VISIBLE,
                xr::SessionState::SYNCHRONIZED,
                xr::SessionState::STOPPING,
                xr::SessionState::EXITING,
                xr::SessionState::LOSS_PENDING,
            ] {
                let mut lifecycle = SessionLifecycle::default();
                lifecycle.on_state_changed(xr::SessionState::READY);
                lifecycle.began();
                lifecycle.on_state_changed(xr::SessionState::FOCUSED);
                let (mut controls, mut tracking) = active_snapshot();
                clear_inactive_input(&lifecycle, &mut controls, &mut tracking);
                assert!(controls.connected && tracking.head.tracked);
                lifecycle.on_state_changed(next);
                clear_inactive_input(&lifecycle, &mut controls, &mut tracking);
                assert_eq!(controls, XrControllerState::default(), "{next:?}");
                assert_eq!(
                    tracking,
                    XrTracking {
                        frame: 7,
                        ..Default::default()
                    },
                    "{next:?}"
                );
            }
        }

        #[test]
        fn image_ownership_failures_are_terminal_and_successful_steps_remain_running() {
            for operation in [
                "xrAcquireSwapchainImage",
                "xrWaitSwapchainImage",
                "xrReleaseSwapchainImage",
                "image lock",
            ] {
                let mut lifecycle = SessionLifecycle::default();
                lifecycle.began();
                lifecycle.on_state_changed(xr::SessionState::FOCUSED);
                let (mut controls, mut tracking) = active_snapshot();
                let failure = if operation == "image lock" {
                    XrError::Render("XR swapchain lock poisoned".into())
                } else {
                    XrError::Runtime {
                        operation,
                        result: sys::Result::ERROR_RUNTIME_FAILURE,
                    }
                };
                let result = guard_image_ownership::<()>(
                    Err(failure.clone()),
                    &mut lifecycle,
                    &mut controls,
                    &mut tracking,
                );
                assert_eq!(result, Err(failure));
                assert!(lifecycle.is_finished());
                assert!(
                    !lifecycle.frame_loop_active(),
                    "a later frame must return before acquire/wait"
                );
                assert!(
                    lifecycle
                        .lost_reason()
                        .unwrap()
                        .contains("recreate the session")
                );
                assert_eq!(controls, XrControllerState::default());
                assert_eq!(
                    tracking,
                    XrTracking {
                        frame: 7,
                        ..Default::default()
                    }
                );
            }
            let mut lifecycle = SessionLifecycle::default();
            lifecycle.began();
            lifecycle.on_state_changed(xr::SessionState::FOCUSED);
            let (mut controls, mut tracking) = active_snapshot();
            // Successful release steps do not change the running lifecycle.
            guard_image_ownership(Ok(()), &mut lifecycle, &mut controls, &mut tracking).unwrap();
            assert!(lifecycle.frame_loop_active());
            assert!(controls.connected && tracking.head.tracked);
        }

        #[test]
        fn instance_loss_and_runtime_loss_invalidate_a_focused_snapshot() {
            for instance_event in [false, true] {
                let mut lifecycle = SessionLifecycle::default();
                lifecycle.began();
                lifecycle.on_state_changed(xr::SessionState::FOCUSED);
                let (mut controls, mut tracking) = active_snapshot();
                if instance_event {
                    lifecycle.on_instance_loss_pending();
                } else {
                    lifecycle.on_lost("xrWaitFrame returned XR_ERROR_SESSION_LOST");
                }
                clear_inactive_input(&lifecycle, &mut controls, &mut tracking);
                assert_eq!(controls, XrControllerState::default());
                assert_eq!(
                    tracking,
                    XrTracking {
                        frame: 7,
                        ..Default::default()
                    }
                );
            }
        }
    }
}
