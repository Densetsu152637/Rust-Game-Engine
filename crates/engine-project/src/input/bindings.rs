use super::*;

pub fn binding_registry() -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    for c in ('A'..='Z').chain('0'..='9') {
        result.insert(format!("Key:{c}"));
    }
    for key in [
        "Space",
        "Apostrophe",
        "Comma",
        "Minus",
        "Period",
        "Slash",
        "Semicolon",
        "Equal",
        "LeftBracket",
        "Backslash",
        "RightBracket",
        "GraveAccent",
        "World1",
        "World2",
        "Escape",
        "Enter",
        "Tab",
        "Backspace",
        "Insert",
        "Delete",
        "Right",
        "Left",
        "Down",
        "Up",
        "PageUp",
        "PageDown",
        "Home",
        "End",
        "CapsLock",
        "ScrollLock",
        "NumLock",
        "PrintScreen",
        "Pause",
        "KeypadDecimal",
        "KeypadDivide",
        "KeypadMultiply",
        "KeypadSubtract",
        "KeypadAdd",
        "KeypadEnter",
        "KeypadEqual",
        "LeftShift",
        "LeftControl",
        "LeftAlt",
        "LeftSuper",
        "RightShift",
        "RightControl",
        "RightAlt",
        "RightSuper",
        "Menu",
    ] {
        result.insert(format!("Key:{key}"));
    }
    for n in 1..=25 {
        result.insert(format!("Key:F{n}"));
    }
    for n in 0..=9 {
        result.insert(format!("Key:Keypad{n}"));
    }
    for key in [
        "Left",
        "Right",
        "Middle",
        "4",
        "5",
        "6",
        "7",
        "8",
        "WheelUp",
        "WheelDown",
        "WheelRight",
        "WheelLeft",
    ] {
        result.insert(format!("Mouse:{key}"));
    }
    for key in [
        "A",
        "B",
        "X",
        "Y",
        "LeftBumper",
        "RightBumper",
        "Back",
        "Start",
        "Guide",
        "LeftThumb",
        "RightThumb",
        "DpadUp",
        "DpadRight",
        "DpadDown",
        "DpadLeft",
        "LeftTrigger",
        "RightTrigger",
    ] {
        result.insert(format!("Gamepad:{key}"));
    }
    for axis in ["LeftX", "LeftY", "RightX", "RightY"] {
        for direction in ["Positive", "Negative"] {
            result.insert(format!("Gamepad:{axis}{direction}"));
        }
    }
    result
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct BindingValidation {
    pub errors: Vec<String>,
    pub conflicts: BTreeMap<String, Vec<String>>,
}
pub fn validate_bindings(bindings: &BindingMap, strict: bool) -> BindingValidation {
    let mut result = BindingValidation::default();
    let registry = binding_registry();
    let mut owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if bindings.len() > 256 {
        result
            .errors
            .push("At most 256 actions are supported".into());
    }
    for (action, tokens) in bindings {
        if action.is_empty() || tokens.is_empty() || tokens.len() > 16 {
            result.errors.push(format!(
                "Action {action} requires a name and 1 to 16 bindings"
            ));
        }
        let mut unique = BTreeSet::new();
        for token in tokens {
            if !registry.contains(token) {
                result.errors.push(format!("Unknown binding: {token}"));
            }
            if !unique.insert(token) {
                result.errors.push(format!("Duplicate binding: {token}"));
            } else {
                owners
                    .entry(token.clone())
                    .or_default()
                    .push(action.clone());
            }
        }
    }
    for (token, actions) in owners {
        if actions.len() > 1 {
            if strict {
                result.errors.push(format!("Binding conflict: {token}"));
            }
            result.conflicts.insert(token, actions);
        }
    }
    result
}
