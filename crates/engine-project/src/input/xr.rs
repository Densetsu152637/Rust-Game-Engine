//! XR controller binding tokens. Authored actions bind `XR:<Hand><Control>`
//! tokens (for example `XR:RightTrigger`, `XR:LeftThumbstickXNegative`) next to
//! keyboard and gamepad tokens. An XR host registers an [`InputSource`] with the
//! ID [`XR_DEVICE_ID`] whose controls are named `<Hand><Control>`; the tokens are
//! resolved against that device by [`InputDevices`] each tick.
use super::*;

/// Stable input-device ID of the XR controller source.
pub const XR_DEVICE_ID: &str = "xr";
/// Prefix of XR binding tokens in authored bindings.
pub const XR_TOKEN_PREFIX: &str = "XR:";
/// Default deadzone of analog triggers and grips.
pub const XR_TRIGGER_DEADZONE: f32 = 0.05;
/// Default deadzone of thumbsticks and trackpads.
pub const XR_THUMBSTICK_DEADZONE: f32 = 0.15;

const HANDS: [&str; 2] = ["Left", "Right"];
/// Analog controls in [0,1]; bound positively only.
const PULLS: [&str; 2] = ["Trigger", "Grip"];
/// Signed analog controls in [-1,1]; tokens carry `Positive`/`Negative`.
const STICK_AXES: [&str; 2] = ["ThumbstickX", "ThumbstickY"];
/// Digital controls.
const BUTTONS: [&str; 4] = ["ThumbstickClick", "Primary", "Secondary", "Menu"];

/// Every XR binding token accepted in authored bindings.
pub fn xr_binding_tokens() -> Vec<String> {
    let mut tokens = Vec::new();
    for hand in HANDS {
        for control in PULLS.iter().chain(BUTTONS.iter()) {
            tokens.push(format!("{XR_TOKEN_PREFIX}{hand}{control}"));
        }
        for axis in STICK_AXES {
            for direction in ["Positive", "Negative"] {
                tokens.push(format!("{XR_TOKEN_PREFIX}{hand}{axis}{direction}"));
            }
        }
    }
    tokens
}

/// Device descriptor of the XR controller source (16 controls: 8 per hand).
pub fn xr_device_descriptor(trigger_deadzone: f32, thumbstick_deadzone: f32) -> DeviceDescriptor {
    let mut controls = BTreeMap::new();
    for hand in HANDS {
        for control in PULLS {
            controls.insert(
                format!("{hand}{control}"),
                ControlKind::Axis {
                    deadzone: trigger_deadzone,
                },
            );
        }
        for axis in STICK_AXES {
            controls.insert(
                format!("{hand}{axis}"),
                ControlKind::Axis {
                    deadzone: thumbstick_deadzone,
                },
            );
        }
        for button in BUTTONS {
            controls.insert(format!("{hand}{button}"), ControlKind::Button);
        }
    }
    DeviceDescriptor {
        id: XR_DEVICE_ID.into(),
        label: "OpenXR controllers".into(),
        controls,
    }
}

/// The device control a token addresses, or `None` for a non-XR or unknown token.
pub fn xr_token_binding(token: &str) -> Option<ControlBinding> {
    let rest = token.strip_prefix(XR_TOKEN_PREFIX)?;
    let hand = HANDS.into_iter().find(|hand| rest.starts_with(hand))?;
    let control = &rest[hand.len()..];
    let binding = |control: &str, direction| ControlBinding {
        device: XR_DEVICE_ID.into(),
        control: format!("{hand}{control}"),
        direction,
    };
    if PULLS.contains(&control) || BUTTONS.contains(&control) {
        return Some(binding(control, AxisDirection::Positive));
    }
    STICK_AXES.into_iter().find_map(|axis| {
        let direction = match control.strip_prefix(axis)? {
            "Positive" => AxisDirection::Positive,
            "Negative" => AxisDirection::Negative,
            _ => return None,
        };
        Some(binding(axis, direction))
    })
}

/// Device bindings implied by the XR tokens of authored action bindings.
pub(super) fn xr_device_bindings(bindings: &BindingMap) -> DeviceBindings {
    bindings
        .iter()
        .filter_map(|(action, tokens)| {
            let controls: Vec<_> = tokens.iter().filter_map(|t| xr_token_binding(t)).collect();
            (!controls.is_empty()).then(|| (action.clone(), controls))
        })
        .collect()
}
