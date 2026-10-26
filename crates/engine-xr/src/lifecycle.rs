//! The OpenXR session state machine, separate from the runtime calls so every
//! transition is testable without a headset. [`crate::XrSession`] feeds it
//! events and performs the returned [`LifecycleAction`].
use crate::status::SessionPhase;
use openxr::SessionState;

/// What the session owner must do after an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleAction {
    None,
    /// Call `xrBeginSession` (state READY).
    BeginSession,
    /// Call `xrEndSession` (state STOPPING).
    EndSession,
    /// Destroy the session (EXITING, or an exit requested before it ran).
    DestroySession,
    /// Session or instance loss; destroy everything and report the reason.
    SessionLost,
}

/// What [`SessionLifecycle::request_exit`] requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitRequest {
    /// The session is running: call `xrRequestExitSession` and keep polling
    /// until the runtime reaches STOPPING then EXITING.
    RequestExitSession,
    /// The session never began (or already ended): destroy it now.
    DestroyNow,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLifecycle {
    phase: SessionPhase,
    begun: bool,
    exit_requested: bool,
    lost: Option<String>,
    destroyed: bool,
}
impl Default for SessionLifecycle {
    fn default() -> Self {
        Self {
            phase: SessionPhase::Idle,
            begun: false,
            exit_requested: false,
            lost: None,
            destroyed: false,
        }
    }
}
impl SessionLifecycle {
    pub fn phase(&self) -> SessionPhase {
        self.phase
    }
    /// `xrBeginSession` succeeded and `xrEndSession` has not run yet.
    pub fn is_begun(&self) -> bool {
        self.begun
    }
    /// The frame loop (`xrWaitFrame`/`xrBeginFrame`/`xrEndFrame`) must run.
    pub fn frame_loop_active(&self) -> bool {
        self.begun && self.lost.is_none() && !self.destroyed
    }
    pub fn lost_reason(&self) -> Option<&str> {
        self.lost.as_deref()
    }
    pub fn exit_requested(&self) -> bool {
        self.exit_requested
    }
    /// The session must be (or has been) destroyed.
    pub fn is_finished(&self) -> bool {
        self.destroyed || self.lost.is_some()
    }

    /// Apply an `XrEventDataSessionStateChanged`.
    pub fn on_state_changed(&mut self, state: SessionState) -> LifecycleAction {
        if self.is_finished() {
            return LifecycleAction::None;
        }
        match state {
            SessionState::IDLE => {
                self.phase = SessionPhase::Idle;
                LifecycleAction::None
            }
            SessionState::READY => {
                self.phase = SessionPhase::Ready;
                if self.exit_requested {
                    self.destroyed = true;
                    LifecycleAction::DestroySession
                } else {
                    LifecycleAction::BeginSession
                }
            }
            SessionState::SYNCHRONIZED => {
                self.phase = SessionPhase::Synchronized;
                LifecycleAction::None
            }
            SessionState::VISIBLE => {
                self.phase = SessionPhase::Visible;
                LifecycleAction::None
            }
            SessionState::FOCUSED => {
                self.phase = SessionPhase::Focused;
                LifecycleAction::None
            }
            SessionState::STOPPING => {
                self.phase = SessionPhase::Stopping;
                if self.begun {
                    LifecycleAction::EndSession
                } else {
                    LifecycleAction::None
                }
            }
            SessionState::EXITING => {
                self.phase = SessionPhase::Exiting;
                self.destroyed = true;
                LifecycleAction::DestroySession
            }
            SessionState::LOSS_PENDING => {
                self.lost = Some(
                    "runtime reported XR_SESSION_STATE_LOSS_PENDING (headset removed or runtime shutting down)"
                        .into(),
                );
                LifecycleAction::SessionLost
            }
            _ => LifecycleAction::None,
        }
    }
    /// Apply `XrEventDataInstanceLossPending`.
    pub fn on_instance_loss_pending(&mut self) -> LifecycleAction {
        if self.lost.is_some() {
            return LifecycleAction::None;
        }
        self.lost = Some("runtime reported XR_TYPE_EVENT_DATA_INSTANCE_LOSS_PENDING (runtime update or shutdown)".into());
        LifecycleAction::SessionLost
    }
    /// A runtime call returned `XR_ERROR_SESSION_LOST` / `XR_ERROR_INSTANCE_LOST`.
    pub fn on_lost(&mut self, reason: impl Into<String>) {
        if self.lost.is_none() {
            self.lost = Some(reason.into());
        }
    }
    /// `xrBeginSession` succeeded.
    pub fn began(&mut self) {
        self.begun = true;
    }
    /// `xrEndSession` succeeded.
    pub fn ended(&mut self) {
        self.begun = false;
    }
    /// The application wants to quit.
    pub fn request_exit(&mut self) -> ExitRequest {
        self.exit_requested = true;
        if self.begun && !self.is_finished() {
            ExitRequest::RequestExitSession
        } else {
            self.destroyed = true;
            ExitRequest::DestroyNow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_normal_session_runs_through_every_state() {
        let mut lifecycle = SessionLifecycle::default();
        assert_eq!(
            lifecycle.on_state_changed(SessionState::IDLE),
            LifecycleAction::None
        );
        assert!(!lifecycle.frame_loop_active());
        assert_eq!(
            lifecycle.on_state_changed(SessionState::READY),
            LifecycleAction::BeginSession
        );
        lifecycle.began();
        assert!(lifecycle.frame_loop_active());
        for (state, phase) in [
            (SessionState::SYNCHRONIZED, SessionPhase::Synchronized),
            (SessionState::VISIBLE, SessionPhase::Visible),
            (SessionState::FOCUSED, SessionPhase::Focused),
        ] {
            assert_eq!(lifecycle.on_state_changed(state), LifecycleAction::None);
            assert_eq!(lifecycle.phase(), phase);
            assert!(phase.is_running());
        }
        assert_eq!(lifecycle.request_exit(), ExitRequest::RequestExitSession);
        assert!(
            lifecycle.frame_loop_active(),
            "frames continue until STOPPING"
        );
        assert_eq!(
            lifecycle.on_state_changed(SessionState::VISIBLE),
            LifecycleAction::None
        );
        assert_eq!(
            lifecycle.on_state_changed(SessionState::STOPPING),
            LifecycleAction::EndSession
        );
        lifecycle.ended();
        assert!(!lifecycle.frame_loop_active());
        assert_eq!(
            lifecycle.on_state_changed(SessionState::IDLE),
            LifecycleAction::None
        );
        assert_eq!(
            lifecycle.on_state_changed(SessionState::EXITING),
            LifecycleAction::DestroySession
        );
        assert_eq!(lifecycle.phase(), SessionPhase::Exiting);
        assert!(lifecycle.is_finished());
        assert_eq!(
            lifecycle.on_state_changed(SessionState::READY),
            LifecycleAction::None
        );
    }

    #[test]
    fn headset_removal_restarts_through_ready() {
        let mut lifecycle = SessionLifecycle::default();
        lifecycle.on_state_changed(SessionState::READY);
        lifecycle.began();
        lifecycle.on_state_changed(SessionState::FOCUSED);
        assert_eq!(
            lifecycle.on_state_changed(SessionState::STOPPING),
            LifecycleAction::EndSession
        );
        lifecycle.ended();
        assert_eq!(
            lifecycle.on_state_changed(SessionState::IDLE),
            LifecycleAction::None
        );
        assert_eq!(
            lifecycle.on_state_changed(SessionState::READY),
            LifecycleAction::BeginSession
        );
    }

    #[test]
    fn exit_before_the_session_runs_destroys_immediately() {
        let mut lifecycle = SessionLifecycle::default();
        assert_eq!(lifecycle.request_exit(), ExitRequest::DestroyNow);
        assert!(lifecycle.is_finished());
        let mut waiting = SessionLifecycle {
            exit_requested: true,
            ..Default::default()
        };
        assert_eq!(
            waiting.on_state_changed(SessionState::READY),
            LifecycleAction::DestroySession
        );
    }

    #[test]
    fn loss_is_terminal_and_keeps_the_first_reason() {
        let mut lifecycle = SessionLifecycle::default();
        lifecycle.on_state_changed(SessionState::READY);
        lifecycle.began();
        assert_eq!(
            lifecycle.on_state_changed(SessionState::LOSS_PENDING),
            LifecycleAction::SessionLost
        );
        assert!(!lifecycle.frame_loop_active());
        assert!(lifecycle.lost_reason().unwrap().contains("LOSS_PENDING"));
        assert_eq!(lifecycle.on_instance_loss_pending(), LifecycleAction::None);
        lifecycle.on_lost("later");
        assert!(lifecycle.lost_reason().unwrap().contains("LOSS_PENDING"));
        let mut instance = SessionLifecycle::default();
        assert_eq!(
            instance.on_instance_loss_pending(),
            LifecycleAction::SessionLost
        );
        assert!(
            instance
                .lost_reason()
                .unwrap()
                .contains("INSTANCE_LOSS_PENDING")
        );
    }
}
