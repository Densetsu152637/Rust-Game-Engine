//! Positional argument and option parsing shared by the commands.

use super::errors::usage;
use engine_core::Result;
use std::ffi::OsString;

/// Parses positional arguments, `--format json` and the listed options. Options
/// whose name is listed take a value, except boolean `--no-*` flags.
pub(super) fn options(
    arguments: &[OsString],
    allowed: &[&str],
) -> Result<(Vec<OsString>, std::collections::BTreeMap<String, String>)> {
    let mut positional = Vec::new();
    let mut flags = std::collections::BTreeMap::new();
    let mut format = false;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        let Some(name) = argument.to_str().filter(|v| v.starts_with("--")) else {
            positional.push(argument.clone());
            index += 1;
            continue;
        };
        if name == "--format" {
            if format || arguments.get(index + 1).is_none_or(|v| v != "json") {
                return Err(usage("Expected one --format json"));
            }
            format = true;
            index += 2;
            continue;
        }
        if !allowed.contains(&name) {
            return Err(usage(format!("Unknown option: {name}")));
        }
        if flags.contains_key(name) {
            return Err(usage(format!("Duplicate option: {name}")));
        }
        if name.starts_with("--no-") {
            flags.insert(name.to_owned(), String::new());
            index += 1;
            continue;
        }
        let value = arguments
            .get(index + 1)
            .and_then(|v| v.to_str())
            .ok_or_else(|| usage(format!("Missing UTF-8 value for {name}")))?;
        flags.insert(name.to_owned(), value.to_owned());
        index += 2;
    }
    Ok((positional, flags))
}
