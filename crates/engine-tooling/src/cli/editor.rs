//! `engine editor`: open the editor on a project manifest.

use super::Outcome;
use super::errors::usage;
use super::options::options;
use engine_core::Result;
use serde_json::json;
use std::ffi::OsString;
use std::path::Path;

pub(super) fn editor(arguments: &[OsString]) -> Result<Outcome> {
    let (args, _) = options(arguments, &[])?;
    if args.len() != 1 {
        return Err(usage("Usage: engine editor <manifest>"));
    }
    engine_editor::run(
        Path::new(&args[0]),
        Box::new(crate::play_host::NativePlayHost::new()),
    )?;
    Ok(Outcome::Json(json!({"closed": true})))
}
