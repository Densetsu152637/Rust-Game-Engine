//! `engine project <action>`: init, validate, assets, shaders and package.

use super::build::{compile_request, prune, rust_crate};
use super::errors::{compiler_diagnostic, usage};
use super::options::options;
use super::{Outcome, USAGE};
use crate::artifacts;
use crate::packaging::macos::{self, MacosTooling};
use crate::packaging::steam::{self, DepotPlan, SteamLibrary};
use crate::packaging::{self, PackageFormat};
use engine_core::Result;
use engine_project::Project;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub(super) fn project(action: &str, arguments: &[OsString]) -> Result<Outcome> {
    match action {
        "init" => {
            let (args, _) = options(arguments, &[])?;
            if args.len() != 2 {
                return Err(usage(
                    "Usage: engine project init <template-manifest> <new-directory>",
                ));
            }
            let manifest = artifacts::initialize_project(Path::new(&args[0]), Path::new(&args[1]))?;
            Ok(Outcome::Json(json!({
                "directory": Path::new(&args[1]),
                "manifest": manifest,
            })))
        }
        "validate" => {
            let (args, flags) = options(arguments, &["--no-rust"])?;
            if args.len() != 1 {
                return Err(usage(
                    "Usage: engine project validate <manifest> [--no-rust]",
                ));
            }
            let project = Project::load(&args[0])?;
            let rust = if flags.contains_key("--no-rust") {
                Value::Null
            } else {
                match rust_crate(&project)? {
                    None => Value::Null,
                    Some(_) => {
                        let (compiler, request) = compile_request(&project)?;
                        let report = compiler.check(&request, &AtomicBool::new(false))?;
                        json!({
                            "manifest": request.project_manifest,
                            "fingerprint": report.fingerprint,
                            "diagnostics": report.diagnostics.iter().map(compiler_diagnostic).collect::<Vec<_>>(),
                        })
                    }
                }
            };
            let mut scenes = vec![json!({"path": project.startup_scene, "id": project.scene.id})];
            for asset in project.assets.values().filter(|a| a.kind == "scene") {
                scenes.push(json!({"path": asset.path, "asset": asset.id}));
            }
            Ok(Outcome::Json(json!({
                "project": project.name,
                "manifest": project.manifest,
                "assets": project.assets.len(),
                "scenes": scenes,
                "rust": rust,
                "diagnostics": [],
            })))
        }
        "assets" => {
            let project = single_manifest(arguments, "assets")?;
            let index = artifacts::asset_index(&project)?;
            let path = artifacts::write_asset_index(&project)?;
            Ok(Outcome::Json(
                json!({"index": path, "assets": index.assets}),
            ))
        }
        "import-model" => {
            let (args, flags) = options(arguments, &["--prefix"])?;
            if args.len() != 2 || !flags.contains_key("--prefix") {
                return Err(usage(
                    "Usage: engine project import-model <manifest> <project-relative-source> --prefix <unique-name>",
                ));
            }
            let project = Project::load(&args[0])?;
            let imported = engine_project::model_import::import_model(
                &project,
                Path::new(&args[1]),
                &flags["--prefix"],
            )?;
            Ok(Outcome::Json(serde_json::to_value(imported)?))
        }
        "shaders" => {
            let project = single_manifest(arguments, "shaders")?;
            let imported = artifacts::import_shaders(&project)?;
            Ok(Outcome::Json(serde_json::to_value(imported)?))
        }
        "package" => {
            // `--format json` stays the global output option; any other value
            // selects the package format.
            let (format, arguments) = package_format(arguments)?;
            let (args, flags) = options(&arguments, &["--profile", "--bundle-id", "--depot-id"])?;
            let apple_host = cfg!(target_os = "macos");
            // Only a macOS depot is an app bundle that takes a bundle identifier.
            let bundled =
                format == PackageFormat::App || (format == PackageFormat::SteamDepot && apple_host);
            if flags.contains_key("--bundle-id") && !bundled {
                return Err(usage(
                    "--bundle-id applies to --format app (and to --format steam-depot on macOS) only",
                ));
            }
            if flags.contains_key("--depot-id") && format != PackageFormat::SteamDepot {
                return Err(usage("--depot-id applies to --format steam-depot only"));
            }
            if args.len() != 2 {
                return Err(usage(
                    "Usage: engine project package <manifest> <destination> [--profile release|dev] [--format dir|zip|tar.gz|appimage|app|steam-depot] [--bundle-id reverse.dns] [--depot-id n]",
                ));
            }
            let release = match flags.get("--profile").map(String::as_str) {
                None | Some("release") => true,
                Some("dev") => false,
                Some(other) => return Err(usage(format!("Unknown profile: {other}"))),
            };
            let depot_id =
                match flags.get("--depot-id") {
                    None => None,
                    Some(value) => Some(value.parse::<u32>().map_err(|_| {
                        usage(format!("--depot-id must be a number, got {value:?}"))
                    })?),
                };
            let project = Project::load(&args[0])?;
            let destination = Path::new(&args[1]);
            // Reject unusable destinations before spending time compiling.
            packaging::check_destination_name(format, destination)?;
            artifacts::check_package_destination(&project, destination)?;
            let bundle_id = flags.get("--bundle-id").map(String::as_str);
            if bundled {
                macos::bundle_identifier(&project.name, bundle_id)?;
            }
            packaging::check_format_available(format)?;
            // Manifest, Steam backend and library are checked before the first compile.
            let target = match format {
                PackageFormat::SteamDepot => {
                    let (plan, library) = steam::check_depot(&project, depot_id)?;
                    AppTarget::Depot(plan, library)
                }
                _ => {
                    steam::require_manifest(&project, steam::BACKEND_COMPILED)?;
                    AppTarget::Bundle(steam::library_for_package(&project)?)
                }
            };
            if bundled {
                return package_app(&project, destination, release, bundle_id, target);
            }
            if rust_crate(&project)?.is_none() {
                return Err(engine_project::Error::new(
                    "project.rust.missing",
                    "Packaging requires the project's Rust gameplay crate",
                )
                .into());
            }
            let (mut compiler, mut request) = compile_request(&project)?;
            request.release = release;
            let generation = compiler.compile(&request, &AtomicBool::new(false))?;
            prune(&request, &generation.directory);
            tracing::info!(
                generation = %generation.generation,
                destination = %destination.display(),
                "packaging project"
            );
            let path = match &target {
                AppTarget::Depot(plan, library) => {
                    let host = engine_platform::PackageHost::current().map_err(|error| {
                        engine_project::Error::new(
                            "package.platform.unsupported",
                            error.to_string(),
                        )
                    })?;
                    packaging::package_steam_depot_with(
                        &project,
                        &generation,
                        destination,
                        &request.engine_root,
                        *plan,
                        &host,
                        library,
                    )?
                }
                AppTarget::Bundle(library) => packaging::package_with_library(
                    &project,
                    &generation,
                    destination,
                    &request.engine_root,
                    format,
                    library.as_ref(),
                )?,
            };
            tracing::info!(path = %path.display(), format = format.name(), "package written");
            let mut result = json!({
                "format": format.name(),
                "generation": generation.generation,
                "fingerprint": generation.fingerprint,
                "profile": if release { "release" } else { "dev" },
                "launchers": ["run.cmd", "run.sh"],
            });
            // The directory format keeps its original `directory` key.
            let key = match format {
                PackageFormat::Dir => "directory",
                PackageFormat::AppImage => "appimage",
                PackageFormat::App => "app",
                PackageFormat::SteamDepot => "depot",
                PackageFormat::Zip | PackageFormat::TarGz => "archive",
            };
            result[key] = json!(path);
            if let Some(steam) = steam_summary(&project, &target) {
                result["steam"] = steam;
            }
            Ok(Outcome::Json(result))
        }
        "" => Err(usage(format!("Expected a project command\n{USAGE}"))),
        other => Err(usage(format!("Unknown project command: {other}\n{USAGE}"))),
    }
}

/// What a package run does with Valve's library: the plain formats stage it when the
/// project is a Steam variant; a `steam-depot` run always has a plan and a library.
enum AppTarget {
    Bundle(Option<SteamLibrary>),
    Depot(DepotPlan, SteamLibrary),
}

/// The `steam` object of the result JSON, present only when a library was staged.
fn steam_summary(project: &Project, target: &AppTarget) -> Option<Value> {
    let (library, depot) = match target {
        AppTarget::Bundle(library) => (library.as_ref()?, None),
        AppTarget::Depot(plan, library) => (library, Some(plan.depot_id)),
    };
    let mut summary = json!({
        "app_id": project.steam.as_ref().map(|steam| steam.app_id),
        "library": {
            "file": library.file,
            "from": library.path,
            "source": library.source.name(),
        },
    });
    if let Some(depot) = depot {
        summary["depot_id"] = json!(depot);
    }
    Some(summary)
}

/// `--format app` (and `steam-depot` on macOS): one generation per Apple
/// architecture, joined by `lipo` into a signed `.app`. The host, tools, bundle
/// identifier and Rust targets are checked before the first compile.
fn package_app(
    project: &Project,
    destination: &Path,
    release: bool,
    bundle_id: Option<&str>,
    target: AppTarget,
) -> Result<Outcome> {
    let tooling = MacosTooling::from_process()?;
    let has_library = match &target {
        AppTarget::Bundle(library) => library.is_some(),
        AppTarget::Depot(..) => true,
    };
    macos::check_steam_bundle(&tooling, has_library)?;
    let bundle_id = macos::bundle_identifier(&project.name, bundle_id)?;
    if rust_crate(project)?.is_none() {
        return Err(engine_project::Error::new(
            "project.rust.missing",
            "Packaging requires the project's Rust gameplay crate",
        )
        .into());
    }
    let (mut compiler, mut request) = compile_request(project)?;
    request.release = release;
    macos::check_targets(&compiler.rustc)?;
    let mut build = |target: &str| {
        let mut request = request.clone();
        request.target = Some(target.to_owned());
        compiler.compile(&request, &AtomicBool::new(false))
    };
    let arm64 = build(macos::ARM64_TARGET)?;
    let x86_64 = build(macos::X86_64_TARGET)?;
    prune(&request, &x86_64.directory);
    tracing::info!(
        arm64 = %arm64.generation,
        x86_64 = %x86_64.generation,
        destination = %destination.display(),
        "packaging project as an app bundle"
    );
    let builds = packaging::AppBuilds {
        arm64: &arm64,
        x86_64: &x86_64,
    };
    let (format, key, path) = match &target {
        AppTarget::Bundle(library) => (
            PackageFormat::App,
            "app",
            packaging::package_app_with_library(
                project,
                &builds,
                destination,
                &request.engine_root,
                Some(&bundle_id),
                &tooling,
                library.as_ref(),
            )?,
        ),
        AppTarget::Depot(plan, library) => (
            PackageFormat::SteamDepot,
            "depot",
            packaging::package_steam_depot_app_with(
                project,
                &builds,
                destination,
                &request.engine_root,
                Some(&bundle_id),
                &tooling,
                *plan,
                library,
            )?,
        ),
    };
    tracing::info!(path = %path.display(), "app bundle written");
    let mut result = json!({
        "format": format.name(),
        "bundle_id": bundle_id,
        "generation": arm64.generation,
        "fingerprint": arm64.fingerprint,
        "x86_64_generation": x86_64.generation,
        "targets": [macos::ARM64_TARGET, macos::X86_64_TARGET],
        "profile": if release { "release" } else { "dev" },
        "signing": if tooling.sign_identity.is_some() { "identity" } else { "ad-hoc" },
        "launchers": [],
    });
    result[key] = json!(path);
    if let Some(steam) = steam_summary(project, &target) {
        result["steam"] = steam;
    }
    Ok(Outcome::Json(result))
}

/// Splits a package `--format <name>` (anything but the global `json`) from the
/// arguments.
fn package_format(arguments: &[OsString]) -> Result<(PackageFormat, Vec<OsString>)> {
    let mut format = None;
    let mut rest = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == "--format" && arguments.get(index + 1).is_none_or(|v| v != "json") {
            let name = arguments
                .get(index + 1)
                .and_then(|v| v.to_str())
                .ok_or_else(|| usage("Missing UTF-8 value for --format"))?;
            let parsed = PackageFormat::parse(name).ok_or_else(|| {
                usage(format!(
                    "Unknown package format: {name} (expected {})",
                    PackageFormat::NAMES
                ))
            })?;
            if format.replace(parsed).is_some() {
                return Err(usage("Duplicate option: --format"));
            }
            index += 2;
        } else {
            rest.push(arguments[index].clone());
            index += 1;
        }
    }
    Ok((format.unwrap_or_default(), rest))
}

fn single_manifest(arguments: &[OsString], action: &str) -> Result<Project> {
    let (args, _) = options(arguments, &[])?;
    if args.len() != 1 {
        return Err(usage(format!("Usage: engine project {action} <manifest>")));
    }
    Ok(Project::load(&args[0])?)
}
