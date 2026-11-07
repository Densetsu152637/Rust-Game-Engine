use engine_project::Project;
use engine_tooling::packaging::{self, PackageFormat};
use engine_tooling::{arguments::parse_run, artifacts::*, compiler::BuildArtifact};
use std::{fs, io::Read, path::Path};

fn fixture(root: &Path) -> Project {
    fs::create_dir(root).unwrap();
    fs::write(root.join("project.json"),r#"{"schema":1,"name":"tooling fixture","startup_scene":"main.scene.json","assets":[{"id":"asset:color","kind":"texture","path":"color.ppm"}]}"#).unwrap();
    fs::write(
        root.join("main.scene.json"),
        r#"{"schema":1,"scene_id":"scene:main","entities":[]}"#,
    )
    .unwrap();
    fs::write(root.join("color.ppm"), "P3\n1 1\n255\n255 255 255\n").unwrap();
    Project::load(root.join("project.json")).unwrap()
}

#[test]
fn index_tracks_stable_id_rename_and_content_changes_and_replaces_cache() {
    let temp = tempfile::tempdir().unwrap();
    let mut project = fixture(&temp.path().join("game"));
    let first = asset_index(&project).unwrap();
    let cache = write_asset_index(&project).unwrap();
    write_asset_index(&project).unwrap();
    let saved: AssetIndex = serde_json::from_slice(&fs::read(cache).unwrap()).unwrap();
    assert_eq!(first, saved);
    fs::rename(
        project.root.join("color.ppm"),
        project.root.join("renamed.ppm"),
    )
    .unwrap();
    project.assets.get_mut("asset:color").unwrap().path = "renamed.ppm".into();
    let moved = asset_index(&project).unwrap();
    assert_eq!(first.assets[0].id, moved.assets[0].id);
    assert_eq!(first.assets[0].content_hash, moved.assets[0].content_hash);
    fs::write(project.root.join("renamed.ppm"), "P3\n1 1\n255\n0 0 0\n").unwrap();
    assert_ne!(
        moved.assets[0].content_hash,
        asset_index(&project).unwrap().assets[0].content_hash
    );
}

#[test]
fn initialize_and_package_refuse_existing_destination_and_leave_authored_data() {
    let temp = tempfile::tempdir().unwrap();
    let project = fixture(&temp.path().join("game"));
    let original = fs::read(&project.manifest).unwrap();
    write_asset_index(&project).unwrap();
    let initialized = temp.path().join("new-game");
    initialize_project(&project.manifest, &initialized).unwrap();
    assert!(!initialized.join(".derived").exists());
    assert!(initialize_project(&project.manifest, &initialized).is_err());
    let generation = temp.path().join("generation");
    fs::create_dir(&generation).unwrap();
    fs::write(generation.join("Cargo.lock"), "version = 4\n").unwrap();
    fs::write(generation.join("cargo-metadata.json"), r#"{"packages":[]}"#).unwrap();
    let engine = temp.path().join("engine");
    fs::create_dir(&engine).unwrap();
    fs::write(engine.join("LICENSE"), "fixture license").unwrap();
    let artifact = BuildArtifact {
        fingerprint: "test".into(),
        generation: "test".into(),
        executable: std::env::current_exe().unwrap(),
        directory: generation,
        diagnostics: vec![],
    };
    let output = temp.path().join("package");
    package_project(&project, &artifact, &output, &engine).unwrap();
    Project::load(output.join("game/project.json")).unwrap();
    assert!(!output.join("game/.derived").exists());
    assert!(
        fs::read_to_string(output.join("run.cmd"))
            .unwrap()
            .contains("%~dp0")
    );
    assert!(output.join("licenses/engine-LICENSE").is_file());
    assert!(output.join("licenses/runtime-Cargo.lock").is_file());
    let host: engine_platform::PackageHost =
        serde_json::from_slice(&fs::read(output.join("package-host.json")).unwrap()).unwrap();
    assert_eq!(host.platform, engine_platform::HostPlatform::current());
    assert!(output.join(host.runtime_filename).is_file());
    assert!(
        fs::read_to_string(output.join("PACKAGE.txt"))
            .unwrap()
            .contains(&host.launcher_filename)
    );
    assert!(package_project(&project, &artifact, &output, &engine).is_err());
    assert_eq!(original, fs::read(project.manifest).unwrap());
}

#[test]
fn run_parser_preserves_namespaced_actions_and_rejects_invalid_runs() {
    let values = [
        "game/project.json",
        "--headless",
        "--ticks",
        "3",
        "--input",
        "ui:confirm:1",
        "--input",
        "move_right:1",
        "--format",
        "json",
    ]
    .map(Into::into);
    let args = parse_run(&values).unwrap();
    assert!(args.headless);
    assert_eq!(args.ticks, 3);
    assert!(args.inputs[&1].contains("ui:confirm"));
    assert!(args.inputs[&1].contains("move_right"));
    for invalid in [
        vec!["project.json", "--ticks", "0"],
        vec!["project.json", "--ticks", "2", "--input", "right:2"],
        vec!["project.json", "--headless", "--headless"],
        vec!["project.json", "--unknown", "value"],
    ] {
        assert!(parse_run(&invalid.into_iter().map(Into::into).collect::<Vec<_>>()).is_err());
    }
}

/// A project plus a fake compiled generation, so packaging runs without Cargo.
struct PackageFixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    project: Project,
    artifact: BuildArtifact,
    engine: std::path::PathBuf,
}

fn package_fixture() -> PackageFixture {
    let temp = tempfile::tempdir().unwrap();
    let project = fixture(&temp.path().join("game"));
    let generation = temp.path().join("generation");
    fs::create_dir(&generation).unwrap();
    fs::write(generation.join("Cargo.lock"), "version = 4\n").unwrap();
    fs::write(generation.join("cargo-metadata.json"), r#"{"packages":[]}"#).unwrap();
    let executable = temp.path().join("fake-runtime");
    fs::write(&executable, "not a real executable").unwrap();
    let engine = temp.path().join("engine");
    fs::create_dir(&engine).unwrap();
    fs::write(engine.join("LICENSE"), "fixture license").unwrap();
    PackageFixture {
        root: temp.path().to_path_buf(),
        project,
        artifact: BuildArtifact {
            fingerprint: "fp".into(),
            generation: "g1".into(),
            executable,
            directory: generation,
            diagnostics: vec![],
        },
        engine,
        _temp: temp,
    }
}

fn try_package(
    fixture: &PackageFixture,
    destination: &Path,
    format: PackageFormat,
) -> engine_project::Result<std::path::PathBuf> {
    packaging::package(
        &fixture.project,
        &fixture.artifact,
        destination,
        &fixture.engine,
        format,
    )
}

fn package_as(fixture: &PackageFixture, name: &str, format: PackageFormat) -> std::path::PathBuf {
    let destination = fixture.root.join(name);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    try_package(fixture, &destination, format).unwrap()
}

fn staging_leftovers(root: &Path) -> Vec<String> {
    fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".engine-export-"))
        .collect()
}

/// `(path, mode & 0o777, is_dir)` per archive entry, in archive order.
type Listing = Vec<(String, u32, bool)>;

fn tar_listing(path: &Path) -> (Listing, Vec<u64>) {
    let file = fs::File::open(path).unwrap();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut listing = Vec::new();
    let mut times = Vec::new();
    for entry in archive.entries().unwrap() {
        let entry = entry.unwrap();
        let header = entry.header();
        listing.push((
            entry.path().unwrap().to_string_lossy().into_owned(),
            header.mode().unwrap() & 0o777,
            header.entry_type().is_dir(),
        ));
        times.push(header.mtime().unwrap());
    }
    (listing, times)
}

fn zip_listing(path: &Path) -> Listing {
    let mut archive = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    (0..archive.len())
        .map(|index| {
            let entry = archive.by_index(index).unwrap();
            (
                entry.name().unwrap().into_owned(),
                entry.unix_mode().expect("unix mode is recorded") & 0o777,
                entry.is_dir(),
            )
        })
        .collect()
}

fn assert_package_listing(listing: &Listing, folder: &str) {
    let runtime = if cfg!(windows) {
        "runtime.exe"
    } else {
        "runtime"
    };
    let names: Vec<&str> = listing
        .iter()
        .map(|(name, ..)| name.trim_end_matches('/'))
        .collect();
    assert_eq!(names[0], folder, "the folder entry comes first");
    assert!(
        names
            .iter()
            .all(|name| name.split('/').next() == Some(folder)),
        "one top-level folder: {names:?}"
    );
    let relative = |name: &str| format!("{folder}/{name}");
    for required in [
        "PACKAGE.txt",
        "game/project.json",
        "game/main.scene.json",
        "game/color.ppm",
        "licenses/engine-LICENSE",
        "licenses/runtime-Cargo.lock",
        "licenses/dependencies.json",
        "generation.json",
        "package-host.json",
        "run.cmd",
        "run.sh",
        runtime,
    ] {
        assert!(names.contains(&relative(required).as_str()), "{required}");
    }
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "entries are sorted by path");
    for (name, mode, is_dir) in listing {
        let name = name.trim_end_matches('/');
        let executable = name == relative(runtime) || name == relative("run.sh");
        let expected = if *is_dir || executable { 0o755 } else { 0o644 };
        assert_eq!(*mode, expected, "{name} {listing:?}");
    }
}

#[test]
fn tar_gz_package_has_one_folder_sorted_entries_fixed_times_and_unix_modes() {
    let fixture = package_fixture();
    let path = package_as(&fixture, "game-1.0.tar.gz", PackageFormat::TarGz);
    assert!(path.is_file());
    let (listing, times) = tar_listing(&path);
    assert_package_listing(&listing, "game-1.0");
    assert!(times.iter().all(|time| *time == 1_577_836_800));
    assert_eq!(staging_leftovers(&fixture.root), Vec::<String>::new());
}

#[test]
fn zip_package_has_one_folder_sorted_entries_unix_modes_and_notes() {
    let fixture = package_fixture();
    let path = package_as(&fixture, "game-1.0.zip", PackageFormat::Zip);
    assert_package_listing(&zip_listing(&path), "game-1.0");
    let mut archive = zip::ZipArchive::new(fs::File::open(&path).unwrap()).unwrap();
    assert_eq!(
        archive.by_name("game-1.0/run.sh").unwrap().compression(),
        zip::CompressionMethod::Deflated
    );
    let mut notes = String::new();
    archive
        .by_name("game-1.0/PACKAGE.txt")
        .unwrap()
        .read_to_string(&mut notes)
        .unwrap();
    assert!(notes.contains("zip archive"), "{notes}");
    assert!(notes.contains("no arguments"), "{notes}");
    assert_eq!(staging_leftovers(&fixture.root), Vec::<String>::new());
}

#[test]
fn archives_are_reproducible_and_directory_notes_name_no_archive() {
    let fixture = package_fixture();
    for (format, name) in [
        (PackageFormat::TarGz, "p.tar.gz"),
        (PackageFormat::Zip, "p.zip"),
    ] {
        let first = package_as(&fixture, &format!("a/{name}"), format);
        let second = package_as(&fixture, &format!("b/{name}"), format);
        assert_eq!(
            fs::read(first).unwrap(),
            fs::read(second).unwrap(),
            "{name}"
        );
    }
    let directory = package_as(&fixture, "dir-package", PackageFormat::Dir);
    let notes = fs::read_to_string(directory.join("PACKAGE.txt")).unwrap();
    assert!(notes.contains("directory package"), "{notes}");
    assert!(!notes.contains("archive"), "{notes}");
}

#[test]
fn archive_destinations_refuse_existing_paths_and_keep_them_intact() {
    let fixture = package_fixture();
    for (format, name) in [
        (PackageFormat::TarGz, "taken.tar.gz"),
        (PackageFormat::Zip, "taken.zip"),
    ] {
        let destination = fixture.root.join(name);
        fs::write(&destination, "precious").unwrap();
        let error = try_package(&fixture, &destination, format).unwrap_err();
        assert_eq!(error.diagnostics[0].code, "tooling.destination.exists");
        assert_eq!(fs::read_to_string(&destination).unwrap(), "precious");
        assert_eq!(
            check_package_destination(&fixture.project, &destination)
                .unwrap_err()
                .diagnostics[0]
                .code,
            "tooling.destination.exists"
        );
        // A directory in the way is refused too.
        let directory = fixture.root.join(format!("{name}.d"));
        fs::create_dir(&directory).unwrap();
        assert!(try_package(&fixture, &directory, format).is_err());
    }
    assert_eq!(staging_leftovers(&fixture.root), Vec::<String>::new());
}

#[test]
fn archive_destination_inside_authored_source_is_refused() {
    let fixture = package_fixture();
    let inside = fixture.project.root.join("package.zip");
    let error = try_package(&fixture, &inside, PackageFormat::Zip).unwrap_err();
    assert_eq!(error.diagnostics[0].code, "tooling.destination.source");
    assert!(!inside.exists());
}

#[cfg(unix)]
#[test]
fn published_archive_and_directory_modes_are_not_private() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = package_fixture();
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    for (format, name) in [
        (PackageFormat::TarGz, "m.tar.gz"),
        (PackageFormat::Zip, "m.zip"),
    ] {
        assert_eq!(mode(&package_as(&fixture, name, format)), 0o644, "{name}");
    }
    assert_eq!(
        mode(&package_as(&fixture, "m-dir", PackageFormat::Dir)),
        0o755
    );
}

#[test]
fn degenerate_archive_names_are_refused_before_any_work() {
    let fixture = package_fixture();
    for (name, format) in [
        ("...zip", PackageFormat::Zip),
        ("..zip", PackageFormat::Zip),
        ("...tar.gz", PackageFormat::TarGz),
    ] {
        let error = try_package(&fixture, &fixture.root.join(name), format).unwrap_err();
        assert_eq!(
            error.diagnostics[0].code, "tooling.destination.invalid",
            "{name}"
        );
        assert!(!fixture.root.join(name).exists());
    }
    assert_eq!(staging_leftovers(&fixture.root), Vec::<String>::new());
}

/// Serialises the tests that write a script and then execute it: another test
/// thread forking while a script is open for writing makes the exec fail with
/// ETXTBSY ("Text file busy"). Held from the script write to the end of the test.
#[cfg(unix)]
static SCRIPT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(unix)]
fn lock_scripts() -> std::sync::MutexGuard<'static, ()> {
    SCRIPT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write an executable script and close the file before it can be executed.
#[cfg(unix)]
fn write_script(path: &Path, script: &str) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let mut file = fs::File::create(path).unwrap();
    file.write_all(script.as_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
fn fake_tool(fixture: &PackageFixture, script: &str) -> packaging::appimage::AppImageTooling {
    let tool = fixture.root.join("fake-appimagetool");
    write_script(&tool, script);
    packaging::appimage::AppImageTooling {
        tool,
        runtime_file: None,
    }
}

#[test]
fn appdir_holds_the_staged_package_and_the_appimage_files() {
    let fixture = package_fixture();
    let appdir = fixture.root.join("First.AppDir");
    fs::create_dir(&appdir).unwrap();
    let id = packaging::stage_appdir(
        &fixture.project,
        &fixture.artifact,
        &appdir,
        &fixture.engine,
    )
    .unwrap();
    assert_eq!(id, packaging::appimage::sanitize_id(&fixture.project.name));
    // The runtime is named `runtime` whatever host generated the AppDir.
    assert_eq!(
        fs::read(appdir.join("runtime")).unwrap(),
        b"not a real executable"
    );
    assert!(appdir.join("game/project.json").is_file());
    assert!(appdir.join("licenses/engine-LICENSE").is_file());
    let run = fs::read_to_string(appdir.join("AppRun")).unwrap();
    assert!(run.starts_with("#!/bin/sh\n"), "{run}");
    assert!(run.contains("exec \"$HERE/runtime\" \"$@\""), "{run}");
    assert!(!run.contains("APPDIR"), "{run}");
    assert!(!run.contains("project.json"), "{run}");
    let desktop = fs::read_to_string(appdir.join(format!("{id}.desktop"))).unwrap();
    for line in [
        "[Desktop Entry]",
        "Type=Application",
        &format!("Name={}", fixture.project.name),
        "Exec=runtime",
        &format!("Icon={id}"),
        "Categories=Game;",
        "Terminal=false",
    ] {
        assert!(desktop.lines().any(|l| l == line), "{line}: {desktop}");
    }
    let icon = fs::read(appdir.join(format!("{id}.png"))).unwrap();
    assert_eq!(&icon[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(fs::read(appdir.join(".DirIcon")).unwrap(), icon);
    let notes = fs::read_to_string(appdir.join("PACKAGE.txt")).unwrap();
    assert!(notes.contains("AppImage"), "{notes}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |name: &str| {
            fs::metadata(appdir.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("AppRun"), 0o755);
        assert_eq!(mode("runtime"), 0o755);
    }
}

#[test]
fn appimage_reports_unsupported_host_and_missing_tool_before_staging() {
    let fixture = package_fixture();
    fn code<T>(result: engine_project::Result<T>) -> String {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(error) => error.diagnostics[0].code.clone(),
        }
    }
    assert_eq!(
        code(packaging::appimage::AppImageTooling::resolve(
            false, None, None, None
        )),
        "tooling.appimage.unsupported_host"
    );
    assert_eq!(
        code(packaging::appimage::AppImageTooling::resolve(
            true, None, None, None
        )),
        "tooling.appimage.tool_missing"
    );
    // The process-level check agrees on a host that is not Linux, before staging.
    if !cfg!(target_os = "linux") {
        assert_eq!(
            code(packaging::check_format_available(PackageFormat::AppImage)),
            "tooling.appimage.unsupported_host"
        );
        let destination = fixture.root.join("g.AppImage");
        assert_eq!(
            code(try_package(&fixture, &destination, PackageFormat::AppImage)),
            "tooling.appimage.unsupported_host"
        );
        assert!(!destination.exists());
    }
    packaging::check_format_available(PackageFormat::Zip).unwrap();
    assert_eq!(staging_leftovers(&fixture.root), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn appimage_runs_the_tool_on_the_appdir_and_publishes_the_output() {
    use std::os::unix::fs::PermissionsExt;
    let _scripts = lock_scripts();
    let fixture = package_fixture();
    let record = fixture.root.join("tool-record");
    // Records the environment and arguments, checks the AppDir, writes the output.
    let tooling = fake_tool(
        &fixture,
        &format!(
            "#!/bin/sh\n\
             echo \"extract=$APPIMAGE_EXTRACT_AND_RUN\" > '{record}'\n\
             echo \"args=$#\" >> '{record}'\n\
             for last; do :; done\n\
             test ! -e \"$last\" || exit 6\n\
             test -x \"$2/AppRun\" || exit 7\n\
             test -x \"$2/runtime\" || exit 8\n\
             ls \"$2\" >> '{record}'\n\
             echo APPIMAGE > \"$last\"\n",
            record = record.display()
        ),
    );
    let destination = fixture.root.join("out/game.AppImage");
    fs::create_dir(fixture.root.join("out")).unwrap();
    let path = packaging::package_appimage(
        &fixture.project,
        &fixture.artifact,
        &destination,
        &fixture.engine,
        &tooling,
    )
    .unwrap();
    assert_eq!(path, destination);
    assert_eq!(fs::read_to_string(&destination).unwrap(), "APPIMAGE\n");
    assert_eq!(
        fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let record = fs::read_to_string(record).unwrap();
    assert!(record.contains("extract=1"), "{record}");
    // --no-appstream, AppDir, output.
    assert!(record.contains("args=3"), "{record}");
    assert!(record.contains("AppRun"), "{record}");
    assert_eq!(
        staging_leftovers(&fixture.root.join("out")),
        Vec::<String>::new()
    );

    // The runtime file is passed to the tool, and an existing destination is kept.
    let runtime = fixture.root.join("type2-runtime");
    fs::write(&runtime, "rt").unwrap();
    let mut with_runtime = tooling.clone();
    with_runtime.runtime_file = Some(runtime.clone());
    write_script(
        &with_runtime.tool,
        "#!/bin/sh\ncase \"$*\" in *--runtime-file*type2-runtime*) ;; *) exit 9;; esac\nfor last; do :; done\necho SECOND > \"$last\"\n",
    );
    let second = fixture.root.join("second.AppImage");
    packaging::package_appimage(
        &fixture.project,
        &fixture.artifact,
        &second,
        &fixture.engine,
        &with_runtime,
    )
    .unwrap();
    let error = packaging::package_appimage(
        &fixture.project,
        &fixture.artifact,
        &second,
        &fixture.engine,
        &with_runtime,
    )
    .unwrap_err();
    assert_eq!(error.diagnostics[0].code, "tooling.destination.exists");
    assert_eq!(fs::read_to_string(&second).unwrap(), "SECOND\n");
}

#[cfg(unix)]
#[test]
fn appimage_tool_failure_publishes_nothing_and_cleans_up() {
    let _scripts = lock_scripts();
    let fixture = package_fixture();
    let destination = fixture.root.join("bad.AppImage");
    for (script, needle) in [
        ("#!/bin/sh\necho boom >&2\nexit 5\n", "boom"),
        ("#!/bin/sh\nexit 0\n", "wrote no file"),
    ] {
        let tooling = fake_tool(&fixture, script);
        let error = packaging::package_appimage(
            &fixture.project,
            &fixture.artifact,
            &destination,
            &fixture.engine,
            &tooling,
        )
        .unwrap_err();
        assert_eq!(error.diagnostics[0].code, "tooling.appimage.failed");
        assert!(error.diagnostics[0].message.contains(needle), "{error:?}");
        assert!(!destination.exists());
    }
    assert_eq!(staging_leftovers(&fixture.root), Vec::<String>::new());
}
