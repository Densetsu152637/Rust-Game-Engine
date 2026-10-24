//! Engine action set and interaction-profile suggested bindings.
//!
//! One action set (`engine`) holds hand-scoped actions (subaction paths
//! `/user/hand/left` and `/user/hand/right`). Each supported interaction
//! profile suggests bindings for the components it has; the runtime picks the
//! profile matching the connected controllers. Values reach engine-project as
//! [`engine_project::xr::XrControllerState`] and `XR:` binding tokens.

/// Name of the engine action set.
pub const ACTION_SET: &str = "engine";
/// Top-level user paths of the two hands, in [`Hand`] order.
pub const HAND_PATHS: [&str; 2] = ["/user/hand/left", "/user/hand/right"];
/// Interaction profiles with suggested bindings.
pub const PROFILES: [&str; 4] = [
    "/interaction_profiles/khr/simple_controller",
    "/interaction_profiles/oculus/touch_controller",
    "/interaction_profiles/valve/index_controller",
    "/interaction_profiles/htc/vive_controller",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Hand {
    Left,
    Right,
}
impl Hand {
    pub const BOTH: [Self; 2] = [Self::Left, Self::Right];
    pub fn path(self) -> &'static str {
        HAND_PATHS[self as usize]
    }
}

/// Value type of an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    Float,
    Vector2,
    Boolean,
    Pose,
    Haptic,
}

/// Engine actions, each created once with both hand subaction paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ActionId {
    Trigger,
    Grip,
    Thumbstick,
    ThumbstickClick,
    Primary,
    Secondary,
    Menu,
    GripPose,
    AimPose,
    Haptic,
}
impl ActionId {
    pub const ALL: [Self; 10] = [
        Self::Trigger,
        Self::Grip,
        Self::Thumbstick,
        Self::ThumbstickClick,
        Self::Primary,
        Self::Secondary,
        Self::Menu,
        Self::GripPose,
        Self::AimPose,
        Self::Haptic,
    ];
    /// OpenXR action name (lowercase, `[a-z0-9_]`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Trigger => "trigger",
            Self::Grip => "grip",
            Self::Thumbstick => "thumbstick",
            Self::ThumbstickClick => "thumbstick_click",
            Self::Primary => "primary",
            Self::Secondary => "secondary",
            Self::Menu => "menu",
            Self::GripPose => "grip_pose",
            Self::AimPose => "aim_pose",
            Self::Haptic => "haptic",
        }
    }
    pub fn localized_name(self) -> &'static str {
        match self {
            Self::Trigger => "Trigger",
            Self::Grip => "Grip",
            Self::Thumbstick => "Thumbstick",
            Self::ThumbstickClick => "Thumbstick click",
            Self::Primary => "Primary button",
            Self::Secondary => "Secondary button",
            Self::Menu => "Menu button",
            Self::GripPose => "Grip pose",
            Self::AimPose => "Aim pose",
            Self::Haptic => "Vibration",
        }
    }
    pub fn kind(self) -> ActionKind {
        match self {
            Self::Trigger | Self::Grip => ActionKind::Float,
            Self::Thumbstick => ActionKind::Vector2,
            Self::ThumbstickClick | Self::Primary | Self::Secondary | Self::Menu => {
                ActionKind::Boolean
            }
            Self::GripPose | Self::AimPose => ActionKind::Pose,
            Self::Haptic => ActionKind::Haptic,
        }
    }
}

/// One suggested binding: an action and a full component path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuggestedBinding {
    pub action: ActionId,
    pub path: String,
}

/// Components of a profile, as `(action, hands, component)`. The component is
/// appended to the hand path, e.g. `/user/hand/left` + `/input/trigger/value`.
type Table = &'static [(ActionId, &'static [Hand], &'static str)];
const BOTH: &[Hand] = &Hand::BOTH;
const LEFT: &[Hand] = &[Hand::Left];
const RIGHT: &[Hand] = &[Hand::Right];

const COMMON_POSES: [(ActionId, &[Hand], &str); 3] = [
    (ActionId::GripPose, BOTH, "/input/grip/pose"),
    (ActionId::AimPose, BOTH, "/input/aim/pose"),
    (ActionId::Haptic, BOTH, "/output/haptic"),
];
const SIMPLE: Table = &[
    (ActionId::Trigger, BOTH, "/input/select/click"),
    (ActionId::Menu, BOTH, "/input/menu/click"),
];
const TOUCH: Table = &[
    (ActionId::Trigger, BOTH, "/input/trigger/value"),
    (ActionId::Grip, BOTH, "/input/squeeze/value"),
    (ActionId::Thumbstick, BOTH, "/input/thumbstick"),
    (ActionId::ThumbstickClick, BOTH, "/input/thumbstick/click"),
    (ActionId::Primary, LEFT, "/input/x/click"),
    (ActionId::Secondary, LEFT, "/input/y/click"),
    (ActionId::Menu, LEFT, "/input/menu/click"),
    (ActionId::Primary, RIGHT, "/input/a/click"),
    (ActionId::Secondary, RIGHT, "/input/b/click"),
];
const INDEX: Table = &[
    (ActionId::Trigger, BOTH, "/input/trigger/value"),
    (ActionId::Grip, BOTH, "/input/squeeze/value"),
    (ActionId::Thumbstick, BOTH, "/input/thumbstick"),
    (ActionId::ThumbstickClick, BOTH, "/input/thumbstick/click"),
    (ActionId::Primary, BOTH, "/input/a/click"),
    (ActionId::Secondary, BOTH, "/input/b/click"),
];
const VIVE: Table = &[
    (ActionId::Trigger, BOTH, "/input/trigger/value"),
    (ActionId::Grip, BOTH, "/input/squeeze/click"),
    (ActionId::Thumbstick, BOTH, "/input/trackpad"),
    (ActionId::ThumbstickClick, BOTH, "/input/trackpad/click"),
    (ActionId::Menu, BOTH, "/input/menu/click"),
];

fn expand(table: &[(ActionId, &[Hand], &str)]) -> Vec<SuggestedBinding> {
    table
        .iter()
        .chain(COMMON_POSES.iter())
        .flat_map(|(action, hands, component)| {
            hands.iter().map(move |hand| SuggestedBinding {
                action: *action,
                path: format!("{}{component}", hand.path()),
            })
        })
        .collect()
}

/// Suggested bindings for `profile` (one of [`PROFILES`]), or `None`.
pub fn suggested_bindings(profile: &str) -> Option<Vec<SuggestedBinding>> {
    let table = match profile {
        "/interaction_profiles/khr/simple_controller" => SIMPLE,
        "/interaction_profiles/oculus/touch_controller" => TOUCH,
        "/interaction_profiles/valve/index_controller" => INDEX,
        "/interaction_profiles/htc/vive_controller" => VIVE,
        _ => return None,
    };
    Some(expand(table))
}

/// OpenXR path syntax: absolute, lowercase `[a-z0-9-_.]` segments, no empty
/// or dot-only segments, below `XR_MAX_PATH_LENGTH` (256).
pub fn is_well_formed_path(path: &str) -> bool {
    path.len() < 256
        && path.starts_with('/')
        && path[1..].split('/').all(|segment| {
            !segment.is_empty()
                && segment.bytes().any(|b| b != b'.')
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.".contains(&b))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_profile_binds_poses_and_haptics_for_both_hands() {
        for profile in PROFILES {
            let bindings = suggested_bindings(profile).unwrap();
            assert!(is_well_formed_path(profile), "{profile}");
            for action in [
                ActionId::GripPose,
                ActionId::AimPose,
                ActionId::Haptic,
                ActionId::Trigger,
            ] {
                for hand in Hand::BOTH {
                    assert!(
                        bindings
                            .iter()
                            .any(|b| b.action == action && b.path.starts_with(hand.path())),
                        "{profile} lacks {action:?} on {hand:?}"
                    );
                }
            }
            let unique: BTreeSet<_> = bindings
                .iter()
                .map(|b| (b.action, b.path.clone()))
                .collect();
            assert_eq!(
                unique.len(),
                bindings.len(),
                "{profile} has duplicate bindings"
            );
            for binding in &bindings {
                assert!(is_well_formed_path(&binding.path), "{}", binding.path);
                let output = binding.path.contains("/output/");
                assert_eq!(
                    output,
                    binding.action.kind() == ActionKind::Haptic,
                    "{}",
                    binding.path
                );
            }
        }
        assert!(suggested_bindings("/interaction_profiles/unknown/thing").is_none());
    }

    #[test]
    fn touch_controller_binds_face_buttons_per_hand() {
        let paths: BTreeSet<_> = suggested_bindings(PROFILES[1])
            .unwrap()
            .into_iter()
            .map(|b| (b.action, b.path))
            .collect();
        for (action, path) in [
            (ActionId::Primary, "/user/hand/left/input/x/click"),
            (ActionId::Secondary, "/user/hand/left/input/y/click"),
            (ActionId::Primary, "/user/hand/right/input/a/click"),
            (ActionId::Secondary, "/user/hand/right/input/b/click"),
            (ActionId::Menu, "/user/hand/left/input/menu/click"),
            (ActionId::Grip, "/user/hand/right/input/squeeze/value"),
            (ActionId::Thumbstick, "/user/hand/left/input/thumbstick"),
        ] {
            assert!(paths.contains(&(action, path.to_owned())), "{path}");
        }
        assert!(
            !paths
                .iter()
                .any(|(_, p)| p == "/user/hand/right/input/menu/click")
        );
        assert_eq!(paths.len(), 9 + 4 + 6);
    }

    #[test]
    fn path_syntax_is_checked() {
        assert!(is_well_formed_path("/user/hand/left/input/trigger/value"));
        for bad in [
            "user/hand",
            "/user//hand",
            "/User/hand",
            "/user/hand/",
            "/user/./x",
            "/user/hand left",
        ] {
            assert!(!is_well_formed_path(bad), "{bad}");
        }
        assert!(
            ActionId::ALL
                .iter()
                .all(|a| is_well_formed_path(&format!("/{}", a.name())))
        );
    }
}
