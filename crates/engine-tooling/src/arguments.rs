use engine_project::{Error, Project, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct RunArguments {
    pub manifest: PathBuf,
    pub headless: bool,
    pub ticks: u64,
    pub inputs: BTreeMap<u64, BTreeSet<String>>,
    pub user_data: Option<PathBuf>,
    pub scene_override: Option<PathBuf>,
}

/// The same parser is used by the compiler launcher and compiled runtime.
pub fn parse_run(arguments: &[OsString]) -> Result<RunArguments> {
    let Some(manifest) = arguments.first() else {
        return Err(Error::new("command.arguments", "Expected project manifest"));
    };
    let mut result = RunArguments {
        manifest: manifest.into(),
        headless: false,
        ticks: 120,
        inputs: BTreeMap::new(),
        user_data: None,
        scene_override: None,
    };
    let mut events = Vec::new();
    let mut seen = BTreeSet::new();
    let mut index = 1;
    while index < arguments.len() {
        let name = arguments[index]
            .to_str()
            .ok_or_else(|| Error::new("command.arguments", "Option must be UTF-8"))?;
        if name != "--input" && !seen.insert(name.to_owned()) {
            return Err(Error::new(
                "command.arguments",
                format!("Duplicate option {name}"),
            ));
        }
        if name == "--headless" {
            result.headless = true;
            index += 1;
            continue;
        }
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| Error::new("command.arguments", format!("Missing value for {name}")))?;
        match name {
            "--ticks" => {
                result.ticks = value
                    .to_str()
                    .and_then(|v| v.parse().ok())
                    .filter(|ticks| (1..=1_000_000).contains(ticks))
                    .ok_or_else(|| {
                        Error::new("command.arguments", "Ticks must be between 1 and 1000000")
                    })?;
            }
            "--input" => {
                let value = value
                    .to_str()
                    .ok_or_else(|| Error::new("command.arguments", "Input must be UTF-8"))?;
                let (action, tick) = value
                    .rsplit_once(':')
                    .ok_or_else(|| Error::new("command.arguments", "Input expects action:tick"))?;
                if action.is_empty() {
                    return Err(Error::new("command.arguments", "Input action is empty"));
                }
                let tick = tick
                    .parse::<u64>()
                    .map_err(|_| Error::new("command.arguments", "Invalid input tick"))?;
                events.push((action.to_owned(), tick));
            }
            "--user-data" => result.user_data = Some(value.into()),
            "--scene-override" => result.scene_override = Some(value.into()),
            "--format" if value == "json" => {}
            _ => {
                return Err(Error::new(
                    "command.arguments",
                    format!("Unknown option or invalid value: {name}"),
                ));
            }
        }
        index += 2;
    }
    for (action, tick) in events {
        if tick >= result.ticks {
            return Err(Error::new(
                "command.arguments",
                "Input tick is outside bounded run",
            ));
        }
        result.inputs.entry(tick).or_default().insert(action);
    }
    Ok(result)
}

pub fn validate_actions(project: &Project, arguments: &RunArguments) -> Result<()> {
    for actions in arguments.inputs.values() {
        for action in actions {
            if !project.input_actions.contains_key(action)
                && !project.input_bindings.contains_key(action)
            {
                return Err(Error::new(
                    "command.input.unknown",
                    format!("Input action is not declared: {action}"),
                ));
            }
        }
    }
    Ok(())
}
