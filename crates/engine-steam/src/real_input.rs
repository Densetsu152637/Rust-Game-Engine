//! Steam Input calls over `steamworks::Input` (steamworks 0.13.1).
//!
//! Notes on the binding: `get_connected_controllers` returns a vector padded to
//! `STEAM_INPUT_MAX_COUNT` with zero handles (its `shrink_to` does not truncate), so the
//! slice variant is used and its count honoured. A handle of `0` means "unknown name".

use std::path::Path;

use steamworks::Client;

use crate::error::{SteamError, SteamResult};
use crate::types::{
    ActionGlyph, ActionOrigin, AnalogActionState, ControllerId, DigitalActionState, InputActionKind,
};

/// `STEAM_INPUT_MAX_COUNT` in the Steamworks SDK (the binding keeps its `sys` module private).
const MAX_CONTROLLERS: usize = 16;

fn reject_nul(text: &str) -> SteamResult<()> {
    if text.contains('\0') {
        Err(SteamError::InvalidName(text.replace('\0', "\\0")))
    } else {
        Ok(())
    }
}

pub(crate) fn init(client: &Client, manifest: Option<&Path>) -> SteamResult<()> {
    let input = client.input();
    if let Some(path) = manifest {
        let text = path
            .to_str()
            .ok_or_else(|| SteamError::InvalidName(path.display().to_string()))?;
        reject_nul(text)?;
        if !input.set_input_action_manifest_file_path(text) {
            return Err(SteamError::Rejected(format!(
                "Steam refused the action manifest {text:?}"
            )));
        }
    }
    if input.init(true) {
        Ok(())
    } else {
        Err(SteamError::Rejected(
            "Steam Input failed to initialise".into(),
        ))
    }
}

pub(crate) fn controllers(client: &Client) -> SteamResult<Vec<ControllerId>> {
    let mut handles = [0_u64; MAX_CONTROLLERS];
    let count = client
        .input()
        .get_connected_controllers_slice(&mut handles[..]);
    Ok(handles[..count.min(handles.len())]
        .iter()
        .copied()
        .filter(|handle| *handle != 0)
        .map(ControllerId)
        .collect())
}

pub(crate) fn activate_action_set(
    client: &Client,
    controller: ControllerId,
    set: &str,
) -> SteamResult<()> {
    reject_nul(set)?;
    let input = client.input();
    let handle = input.get_action_set_handle(set);
    if handle == 0 {
        return Err(SteamError::NotFound(format!("action set {set:?}")));
    }
    input.activate_action_set_handle(controller.0, handle);
    Ok(())
}

pub(crate) fn digital(
    client: &Client,
    controller: ControllerId,
    action: &str,
) -> SteamResult<DigitalActionState> {
    reject_nul(action)?;
    let input = client.input();
    let handle = input.get_digital_action_handle(action);
    if handle == 0 {
        return Err(SteamError::NotFound(format!("digital action {action:?}")));
    }
    let data = input.get_digital_action_data(controller.0, handle);
    Ok(DigitalActionState {
        down: data.bState,
        active: data.bActive,
    })
}

pub(crate) fn analog(
    client: &Client,
    controller: ControllerId,
    action: &str,
) -> SteamResult<AnalogActionState> {
    reject_nul(action)?;
    let input = client.input();
    let handle = input.get_analog_action_handle(action);
    if handle == 0 {
        return Err(SteamError::NotFound(format!("analog action {action:?}")));
    }
    let data = input.get_analog_action_data(controller.0, handle);
    Ok(AnalogActionState {
        x: data.x,
        y: data.y,
        active: data.bActive,
    })
}

pub(crate) fn glyphs(
    client: &Client,
    controller: ControllerId,
    set: &str,
    action: &str,
    kind: InputActionKind,
) -> SteamResult<Vec<ActionGlyph>> {
    reject_nul(set)?;
    reject_nul(action)?;
    let input = client.input();
    let set_handle = input.get_action_set_handle(set);
    if set_handle == 0 {
        return Err(SteamError::NotFound(format!("action set {set:?}")));
    }
    let origins = match kind {
        InputActionKind::Digital => {
            let handle = input.get_digital_action_handle(action);
            if handle == 0 {
                return Err(SteamError::NotFound(format!("digital action {action:?}")));
            }
            input.get_digital_action_origins(controller.0, set_handle, handle)
        }
        InputActionKind::Analog => {
            let handle = input.get_analog_action_handle(action);
            if handle == 0 {
                return Err(SteamError::NotFound(format!("analog action {action:?}")));
            }
            input.get_analog_action_origins(controller.0, set_handle, handle)
        }
    };
    Ok(origins
        .into_iter()
        .map(|origin| ActionGlyph {
            origin: ActionOrigin(origin as u32),
            name: input.get_string_for_action_origin(origin),
            glyph_path: input.get_glyph_for_action_origin(origin),
        })
        .collect())
}
