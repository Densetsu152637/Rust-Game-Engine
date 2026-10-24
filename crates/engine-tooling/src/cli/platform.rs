use super::{Outcome, errors::usage, options::options};
use engine_core::Result;
use engine_platform::HostPlatform;
use serde_json::json;
use std::ffi::OsString;

pub(super) fn platform(arguments: &[OsString]) -> Result<Outcome> {
    let (positional, flags) = options(arguments, &["--target"])?;
    if !positional.is_empty() {
        return Err(usage("platform accepts only --target and --format json"));
    }
    let target = flags
        .get("--target")
        .map(|value| value.parse::<HostPlatform>().map_err(usage))
        .transpose()?;
    let platforms = match target {
        Some(platform) => vec![platform.capabilities()],
        None => HostPlatform::ALL
            .into_iter()
            .map(HostPlatform::capabilities)
            .collect(),
    };
    Ok(Outcome::Json(
        json!({"host": {"platform": HostPlatform::current(), "architecture": std::env::consts::ARCH}, "platforms": platforms}),
    ))
}
