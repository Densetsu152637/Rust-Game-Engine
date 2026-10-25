use engine_desktop::WgpuBackends;
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
    pub events: engine_project::EventSchedule,
    pub user_data: Option<PathBuf>,
    pub scene_override: Option<PathBuf>,
    /// Render the final frame offscreen to this PNG after the last tick.
    pub screenshot: Option<PathBuf>,
    /// Screenshot size in pixels; `[640, 360]` unless `--screenshot-size` is given.
    pub screenshot_size: [u32; 2],
    /// wgpu graphics API from `--render-backend`; `None` leaves the choice to
    /// `ENGINE_RENDER_BACKEND` and then to the platform default. Headless runs
    /// never create a device and ignore it.
    pub render_backend: Option<WgpuBackends>,
    /// Acoustic propagation tier from `--acoustics-backend`; replaces the scene's
    /// authored `AcousticEnvironment.backend` for the run. `None` keeps it.
    pub acoustics_backend: Option<engine_desktop::BackendPreference>,
}

/// Default offscreen screenshot size.
pub const DEFAULT_SCREENSHOT_SIZE: [u32; 2] = [640, 360];
/// Largest accepted screenshot edge in pixels.
pub const MAX_SCREENSHOT_EDGE: u32 = 4096;

fn parse_size(text: &str) -> Option<[u32; 2]> {
    let (width, height) = text.split_once('x')?;
    let size = [width.parse().ok()?, height.parse().ok()?];
    size.iter()
        .all(|edge| (1..=MAX_SCREENSHOT_EDGE).contains(edge))
        .then_some(size)
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
        events: Default::default(),
        user_data: None,
        scene_override: None,
        screenshot: None,
        screenshot_size: DEFAULT_SCREENSHOT_SIZE,
        render_backend: None,
        acoustics_backend: None,
    };
    let mut size_given = false;
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
            "--events" => {
                result.events = engine_project::EventSchedule::load(&PathBuf::from(value))?
            }
            "--user-data" => result.user_data = Some(value.into()),
            "--scene-override" => result.scene_override = Some(value.into()),
            "--screenshot" => result.screenshot = Some(value.into()),
            "--render-backend" => {
                result.render_backend =
                    Some(value.to_str().and_then(|v| v.parse().ok()).ok_or_else(|| {
                        Error::new(
                            "command.arguments",
                            "Render backend must be primary, vulkan, dx12, metal, gl or all",
                        )
                    })?);
            }
            "--acoustics-backend" => {
                result.acoustics_backend = Some(
                    value
                        .to_str()
                        .and_then(|v| engine_desktop::parse_acoustics_backend(v).ok())
                        .ok_or_else(|| {
                            Error::new(
                                "command.arguments",
                                "Acoustics backend must be auto, ray_query, compute or cpu",
                            )
                        })?,
                );
            }
            "--screenshot-size" => {
                result.screenshot_size = value.to_str().and_then(parse_size).ok_or_else(|| {
                    Error::new(
                        "command.arguments",
                        format!(
                            "Screenshot size must be WIDTHxHEIGHT, each 1 to {MAX_SCREENSHOT_EDGE}"
                        ),
                    )
                })?;
                size_given = true;
            }
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
    if size_given && result.screenshot.is_none() {
        return Err(Error::new(
            "command.arguments",
            "--screenshot-size requires --screenshot",
        ));
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
    result.events.validate_run(result.ticks)?;
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
