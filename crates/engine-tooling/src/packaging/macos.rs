//! macOS `.app` bundle output: the staged package arranged as an application
//! bundle, a universal (arm64 + x86_64) runtime and a code signature.
//!
//! The file generators ([`info_plist`], [`default_icns`], [`bundle_identifier`],
//! [`write_bundle_files`]) are pure and run on every OS. Only [`make_universal`],
//! [`sign_bundle`] and [`check_targets`] run macOS tools; [`MacosTooling::resolve`]
//! checks the host and the tools before anything is compiled or staged.
//!
//! Bundle layout:
//!
//! ```text
//! <Name>.app/Contents/Info.plist, PkgInfo
//! <Name>.app/Contents/MacOS/runtime                universal executable
//! <Name>.app/Contents/Resources/game/              project content
//! <Name>.app/Contents/Resources/licenses/          engine and dependency notices
//! <Name>.app/Contents/Resources/<id>.icns          the application icon
//! <Name>.app/Contents/Resources/{generation.json,package-host.json,PACKAGE.txt}
//! <Name>.app/Contents/Frameworks/                  nested code (empty for now)
//! ```

use super::appimage::{default_icon, sanitize_id};
use engine_project::{Error, Result};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Environment variable naming the signing identity (a `Developer ID Application`
/// certificate name or SHA-1 hash in the keychain). Unset or empty means ad-hoc.
pub const SIGN_IDENTITY_ENV: &str = "ENGINE_MACOS_SIGN_IDENTITY";
/// Rust target of the Apple silicon slice.
pub const ARM64_TARGET: &str = "aarch64-apple-darwin";
/// Rust target of the Intel slice.
pub const X86_64_TARGET: &str = "x86_64-apple-darwin";
/// Prefix of the default bundle identifier; the sanitised project id follows.
pub const DEFAULT_BUNDLE_ID_PREFIX: &str = "local.engine.";
/// The manifest has no version field yet.
const DEFAULT_VERSION: &str = "0.1.0";
const DEFAULT_BUILD: &str = "1";
/// `rustc --print deployment-target` (unstable, `RUSTC_BOOTSTRAP=1 rustc -Z
/// unstable-options`) reports `MACOSX_DEPLOYMENT_TARGET=11.0` for
/// `aarch64-apple-darwin` and `10.12` for `x86_64-apple-darwin` on the pinned 1.99.1
/// toolchain. One value must be true for both slices, so the higher is used.
const MIN_SYSTEM_VERSION: &str = "11.0";
/// Apple documents `CFBundleName` as a short name of fewer than 16 characters.
const BUNDLE_NAME_LENGTH: usize = 15;
const MAX_BUNDLE_ID_LENGTH: usize = 255;
const PKG_INFO: &str = "APPL????";
/// Icon entry type for a 256x256 PNG in the Apple Icon Image format (`ic08`: 256x256,
/// PNG or JPEG 2000 data; table in the "Apple Icon Image format" article on
/// Wikipedia, which the macOS CI job cross-checks with `sips`).
const ICNS_256_PNG: &[u8; 4] = b"ic08";

/// The macOS host and tools a build needs, validated.
#[derive(Clone, Debug)]
pub struct MacosTooling {
    pub lipo: PathBuf,
    /// Strips extended attributes before signing.
    pub xattr: PathBuf,
    pub codesign: PathBuf,
    /// A signing identity, or `None` for an ad-hoc signature.
    pub sign_identity: Option<String>,
}

impl MacosTooling {
    /// Resolve from the process: macOS host, `lipo` and `codesign` on `PATH`, and
    /// `ENGINE_MACOS_SIGN_IDENTITY`.
    pub fn from_process() -> Result<Self> {
        Self::resolve(
            cfg!(target_os = "macos"),
            std::env::var_os("PATH").as_deref(),
            std::env::var_os(SIGN_IDENTITY_ENV).as_deref(),
        )
    }

    /// The host and tool lookup with every input injected.
    ///
    /// - `tooling.app.unsupported_host`: not a macOS host.
    /// - `tooling.app.tool_missing`: `lipo`, `xattr` or `codesign` is not on `PATH`.
    /// - `tooling.app.identity_invalid`: the identity is not UTF-8.
    pub fn resolve(
        macos_host: bool,
        path_env: Option<&OsStr>,
        identity_env: Option<&OsStr>,
    ) -> Result<Self> {
        if !macos_host {
            return Err(Error::new(
                "tooling.app.unsupported_host",
                "--format app is built on a macOS host only (it needs lipo and codesign)",
            ));
        }
        let find = |name: &str| {
            path_env
                .into_iter()
                .flat_map(std::env::split_paths)
                .map(|directory| directory.join(name))
                .find(|candidate| candidate.is_file())
                .ok_or_else(|| {
                    Error::new(
                        "tooling.app.tool_missing",
                        format!("{name} was not found on PATH"),
                    )
                })
        };
        let lipo = find("lipo")?;
        let xattr = find("xattr")?;
        let codesign = find("codesign")?;
        let sign_identity = match identity_env.filter(|value| !value.is_empty()) {
            Some(value) => Some(
                value
                    .to_str()
                    .ok_or_else(|| {
                        Error::new(
                            "tooling.app.identity_invalid",
                            format!("{SIGN_IDENTITY_ENV} is not valid UTF-8"),
                        )
                    })?
                    .to_owned(),
            ),
            None => None,
        };
        Ok(Self {
            lipo,
            xattr,
            codesign,
            sign_identity,
        })
    }
}

/// Fail with `tooling.app.target_missing` unless both Rust targets are installed
/// for `rustc`. A missing target has no `rustlib/<target>/lib` directory. A
/// `rustc` that cannot be started is `tooling.app.toolchain_failed`.
pub fn check_targets(rustc: &Path) -> Result<()> {
    for target in [ARM64_TARGET, X86_64_TARGET] {
        let mut command = Command::new(rustc);
        command
            .args(["--print", "target-libdir", "--target", target])
            .stdin(Stdio::null());
        // Rustup proxies pick the toolchain from the working directory; use the
        // compiler's own, as the build does.
        if let Some(parent) = rustc.parent().filter(|p| p.is_dir()) {
            command.current_dir(parent);
        }
        let output = command.output().map_err(|error| {
            Error::new(
                "tooling.app.toolchain_failed",
                format!("Could not run {}: {error}", rustc.display()),
            )
        })?;
        let directory = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !output.status.success() || directory.is_empty() || !Path::new(&directory).is_dir() {
            return Err(Error::new(
                "tooling.app.target_missing",
                format!("Rust target {target} is not installed; run: rustup target add {target}"),
            ));
        }
    }
    Ok(())
}

/// The bundle name of `destination`: its file name without the `.app` suffix
/// (matched case-insensitively). A destination without the suffix, or with
/// nothing before it, is refused with `tooling.destination.invalid`.
pub fn bundle_file_stem(destination: &Path) -> Result<String> {
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            Error::new(
                "tooling.destination.invalid",
                "Destination requires a UTF-8 file name",
            )
        })?;
    const SUFFIX: &str = ".app";
    let stem = name
        .len()
        .checked_sub(SUFFIX.len())
        .filter(|&split| name.is_char_boundary(split))
        .filter(|&split| name[split..].eq_ignore_ascii_case(SUFFIX))
        .map(|split| &name[..split])
        .ok_or_else(|| {
            Error::new(
                "tooling.destination.invalid",
                "An application bundle destination must end in .app",
            )
        })?;
    if stem.is_empty() || matches!(stem, "." | "..") {
        return Err(Error::new(
            "tooling.destination.invalid",
            "The bundle name before .app must not be empty, . or ..",
        ));
    }
    Ok(stem.to_owned())
}

/// The `CFBundleIdentifier`: `explicit` when given (validated), else
/// `local.engine.<id>` where `<id>` is [`sanitize_id`] of the project name.
/// The default only uses `local.engine.` so it never claims a real domain; pass
/// `--bundle-id` with a domain you own for anything distributed.
pub fn bundle_identifier(project_name: &str, explicit: Option<&str>) -> Result<String> {
    match explicit {
        Some(value) => validate_bundle_id(value).map(|()| value.to_owned()),
        None => Ok(format!(
            "{DEFAULT_BUNDLE_ID_PREFIX}{}",
            sanitize_id(project_name)
        )),
    }
}

/// Reverse-DNS rule: at most 255 characters; two or more dot-separated components;
/// each non-empty ASCII letters, digits and `-` (the characters Apple allows in
/// `CFBundleIdentifier`) and neither starting nor ending in `-`.
/// Refused with `tooling.app.bundle_id_invalid`.
pub fn validate_bundle_id(value: &str) -> Result<()> {
    let invalid = |reason: &str| {
        Error::new(
            "tooling.app.bundle_id_invalid",
            format!("Bundle identifier {value:?} is invalid: {reason}"),
        )
    };
    if value.len() > MAX_BUNDLE_ID_LENGTH {
        return Err(invalid("longer than 255 characters"));
    }
    let components: Vec<&str> = value.split('.').collect();
    if components.len() < 2 {
        return Err(invalid("needs at least two dot-separated components"));
    }
    for component in components {
        if component.is_empty() {
            return Err(invalid("empty component"));
        }
        if !component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(invalid(
                "components may only contain ASCII letters, digits and -",
            ));
        }
        if component.starts_with('-') || component.ends_with('-') {
            return Err(invalid("a component may not start or end with -"));
        }
    }
    Ok(())
}

/// Escape text for an XML 1.0 element. Characters XML cannot carry (control
/// characters other than tab, line feed and carriage return) become spaces.
pub fn xml_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            '\t' | '\n' | '\r' => escaped.push(character),
            character if character.is_control() || matches!(character, '\u{FFFE}' | '\u{FFFF}') => {
                escaped.push(' ')
            }
            character => escaped.push(character),
        }
    }
    escaped
}

/// The text of `Contents/Info.plist`. `id` names the icon file; `display_name` is
/// the project name (falls back to `id` when blank).
pub fn info_plist(id: &str, display_name: &str, bundle_id: &str) -> String {
    let single_line: String = display_name
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let display = single_line.trim();
    let display = if display.is_empty() { id } else { display };
    let short: String = display.chars().take(BUNDLE_NAME_LENGTH).collect();
    let short = short.trim_end().to_owned();
    let entries: [(&str, PlistValue); 14] = [
        ("CFBundleDevelopmentRegion", PlistValue::Text("en")),
        ("CFBundleExecutable", PlistValue::Text("runtime")),
        ("CFBundleIconFile", PlistValue::Owned(format!("{id}.icns"))),
        ("CFBundleIdentifier", PlistValue::Text(bundle_id)),
        ("CFBundleInfoDictionaryVersion", PlistValue::Text("6.0")),
        ("CFBundleName", PlistValue::Owned(short)),
        ("CFBundleDisplayName", PlistValue::Text(display)),
        ("CFBundlePackageType", PlistValue::Text("APPL")),
        (
            "CFBundleShortVersionString",
            PlistValue::Text(DEFAULT_VERSION),
        ),
        ("CFBundleVersion", PlistValue::Text(DEFAULT_BUILD)),
        (
            "LSApplicationCategoryType",
            PlistValue::Text("public.app-category.games"),
        ),
        (
            "LSMinimumSystemVersion",
            PlistValue::Text(MIN_SYSTEM_VERSION),
        ),
        ("NSHighResolutionCapable", PlistValue::True),
        ("NSPrincipalClass", PlistValue::Text("NSApplication")),
    ];
    let mut plist = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n",
    );
    for (key, value) in entries {
        plist.push_str(&format!("\t<key>{key}</key>\n"));
        match value {
            PlistValue::Text(text) => {
                plist.push_str(&format!("\t<string>{}</string>\n", xml_escape(text)));
            }
            PlistValue::Owned(text) => {
                plist.push_str(&format!("\t<string>{}</string>\n", xml_escape(&text)));
            }
            PlistValue::True => plist.push_str("\t<true/>\n"),
        }
    }
    plist.push_str("</dict>\n</plist>\n");
    plist
}

enum PlistValue<'a> {
    Text(&'a str),
    Owned(String),
    True,
}

/// An Apple Icon Image (`.icns`) holding one `ic08` entry that carries `png`
/// unchanged: the `icns` magic and total length (big-endian `u32`, header
/// included), then the entry type, the entry length (header included) and the data.
pub fn icns(png: &[u8]) -> Vec<u8> {
    let entry_length = (8 + png.len()) as u32;
    let total_length = 8 + entry_length;
    let mut bytes = Vec::with_capacity(total_length as usize);
    bytes.extend_from_slice(b"icns");
    bytes.extend_from_slice(&total_length.to_be_bytes());
    bytes.extend_from_slice(ICNS_256_PNG);
    bytes.extend_from_slice(&entry_length.to_be_bytes());
    bytes.extend_from_slice(png);
    bytes
}

/// The default engine icon as an `.icns`.
pub fn default_icns() -> Result<Vec<u8>> {
    Ok(icns(&default_icon()?))
}

/// Add the bundle files to a bundle that already holds the staged package:
/// `Info.plist`, `PkgInfo`, the icon and an empty `Frameworks/`.
pub fn write_bundle_files(bundle: &Path, display_name: &str, bundle_id: &str) -> Result<()> {
    let id = sanitize_id(display_name);
    let contents = bundle.join("Contents");
    fs::write(
        contents.join("Info.plist"),
        info_plist(&id, display_name, bundle_id),
    )?;
    fs::write(contents.join("PkgInfo"), PKG_INFO)?;
    fs::write(
        contents.join("Resources").join(format!("{id}.icns")),
        default_icns()?,
    )?;
    fs::create_dir_all(contents.join("Frameworks"))?;
    Ok(())
}

/// Join the two single-architecture runtimes into the bundle's
/// `Contents/MacOS/runtime` with `lipo -create`. Fails with
/// `tooling.app.lipo_failed`.
pub fn make_universal(
    tooling: &MacosTooling,
    arm64: &Path,
    x86_64: &Path,
    bundle: &Path,
) -> Result<()> {
    let directory = bundle.join("Contents/MacOS");
    let joined = directory.join("runtime.universal");
    let mut command = Command::new(&tooling.lipo);
    command
        .arg("-create")
        .arg("-output")
        .arg(&joined)
        .arg(arm64)
        .arg(x86_64);
    run_tool(command, "tooling.app.lipo_failed", "lipo", Some(&joined))?;
    fs::rename(&joined, directory.join("runtime"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory.join("runtime"), fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Remove every extended attribute from the staged bundle with `xattr -cr`.
/// Finder metadata or resource forks copied from the project would make
/// `codesign` refuse with "detritus not allowed". Fails with
/// `tooling.app.xattr_failed`.
pub fn strip_extended_attributes(tooling: &MacosTooling, bundle: &Path) -> Result<()> {
    let mut command = Command::new(&tooling.xattr);
    command.arg("-cr").arg(bundle);
    run_tool(command, "tooling.app.xattr_failed", "xattr", None)
}

/// The `codesign` arguments before the path: ad-hoc without a timestamp, or the
/// identity with the hardened runtime and a secure timestamp.
pub fn codesign_arguments(identity: Option<&str>) -> Vec<OsString> {
    let mut arguments: Vec<OsString> = vec!["--force".into(), "--sign".into()];
    match identity {
        None => {
            arguments.push("-".into());
            arguments.push("--timestamp=none".into());
        }
        Some(identity) => {
            arguments.push(identity.into());
            arguments.extend(["--options", "runtime", "--timestamp"].map(OsString::from));
        }
    }
    arguments
}

/// Sign the bundle: each top-level item of `Contents/Frameworks` (nested code)
/// first, in name order, then the bundle itself. Never `--deep`. Fails with
/// `tooling.app.codesign_failed`.
pub fn sign_bundle(tooling: &MacosTooling, bundle: &Path) -> Result<()> {
    let mut nested = Vec::new();
    let frameworks = bundle.join("Contents/Frameworks");
    if frameworks.is_dir() {
        for entry in fs::read_dir(&frameworks)? {
            nested.push(entry?.path());
        }
        nested.sort();
    }
    for path in nested.iter().map(PathBuf::as_path).chain([bundle]) {
        let mut command = Command::new(&tooling.codesign);
        command
            .args(codesign_arguments(tooling.sign_identity.as_deref()))
            .arg(path);
        run_tool(command, "tooling.app.codesign_failed", "codesign", None)?;
    }
    Ok(())
}

fn run_tool(
    mut command: Command,
    code: &str,
    name: &str,
    expect_file: Option<&Path>,
) -> Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let result = command
        .output()
        .map_err(|error| Error::new(code, format!("Could not run {name}: {error}")))?;
    let produced = expect_file.is_none_or(|path| fs::metadata(path).is_ok_and(|m| m.len() > 0));
    if result.status.success() && produced {
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&result.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&result.stdout));
    let tail = text.trim();
    let start = tail.len().saturating_sub(2000);
    let start = (start..=tail.len())
        .find(|&index| tail.is_char_boundary(index))
        .unwrap_or(tail.len());
    Err(Error::new(
        code,
        format!(
            "{name} {}: {}",
            if result.status.success() {
                "wrote no file".to_owned()
            } else {
                format!("exited with {}", result.status)
            },
            &tail[start..]
        ),
    ))
}

/// Build the bundle with `populate` inside a staging directory next to
/// `destination`, then publish it with a rename only when complete. The bundle is
/// staged under its final file name because `codesign` only treats a directory
/// named `*.app` as a bundle. An existing destination is refused before and again
/// when publishing.
pub fn export_new_app(
    destination: &Path,
    populate: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    bundle_file_stem(destination)?;
    if destination.exists() {
        return Err(exists("Destination must be new"));
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    let staging = tempfile::Builder::new()
        .prefix(".engine-export-")
        .tempdir_in(&parent)?;
    let bundle = staging.path().join(
        destination
            .file_name()
            .expect("bundle_file_stem checked the file name"),
    );
    fs::create_dir(&bundle)?;
    populate(&bundle)?;
    if destination.exists() {
        return Err(exists("Destination appeared during export"));
    }
    fs::rename(&bundle, destination)?;
    Ok(())
}

fn exists(message: &str) -> Error {
    Error::new("tooling.destination.exists", message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code<T>(result: Result<T>) -> String {
        result.err().expect("expected an error").diagnostics[0]
            .code
            .clone()
    }

    #[test]
    fn plist_holds_the_required_keys_in_a_fixed_order() {
        let plist = info_plist(
            "first-project",
            "First Project",
            "local.engine.first-project",
        );
        assert!(plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
        assert!(plist.ends_with("</dict>\n</plist>\n"));
        for pair in [
            ("CFBundleExecutable", "runtime"),
            ("CFBundleIdentifier", "local.engine.first-project"),
            ("CFBundleName", "First Project"),
            ("CFBundleDisplayName", "First Project"),
            ("CFBundleShortVersionString", "0.1.0"),
            ("CFBundleVersion", "1"),
            ("CFBundlePackageType", "APPL"),
            ("CFBundleIconFile", "first-project.icns"),
            ("LSMinimumSystemVersion", "11.0"),
            ("LSApplicationCategoryType", "public.app-category.games"),
        ] {
            assert!(
                plist.contains(&format!(
                    "<key>{}</key>\n\t<string>{}</string>",
                    pair.0, pair.1
                )),
                "{pair:?}: {plist}"
            );
        }
        assert!(plist.contains("<key>NSHighResolutionCapable</key>\n\t<true/>"));
        assert_eq!(
            plist,
            info_plist(
                "first-project",
                "First Project",
                "local.engine.first-project"
            )
        );
    }

    #[test]
    fn plist_escapes_values_and_shortens_the_bundle_name() {
        let plist = info_plist("a", "A & <B> \"C\" 'D'\u{1}\nE", "x.y");
        assert!(
            plist.contains("<string>A &amp; &lt;B&gt; &quot;C&quot; &apos;D&apos;  E</string>"),
            "{plist}"
        );
        assert!(!plist.contains('\u{1}'));
        // Name: fewer than 16 characters; display name keeps the whole text.
        let plist = info_plist("x", "Extremely Long Project Name", "x.y");
        assert!(plist.contains("<key>CFBundleName</key>\n\t<string>Extremely Long</string>"));
        assert!(plist.contains("<string>Extremely Long Project Name</string>"));
        // A space left at the end by the cut is dropped.
        let plist = info_plist("x", "12345678901234 tail", "x.y");
        assert!(plist.contains("<key>CFBundleName</key>\n\t<string>12345678901234</string>"));
        assert_eq!(xml_escape("a\u{FFFE}b\u{FFFF}c"), "a b c");
        // A blank name falls back to the id.
        let plist = info_plist("game", "  ", "x.y");
        assert!(plist.contains("<key>CFBundleDisplayName</key>\n\t<string>game</string>"));
        assert_eq!(xml_escape("日本語 é"), "日本語 é");
    }

    #[test]
    fn bundle_identifiers_default_from_the_sanitised_name_and_validate_overrides() {
        assert_eq!(
            bundle_identifier("First Project", None).unwrap(),
            "local.engine.first-project"
        );
        assert_eq!(
            bundle_identifier("日本語", None).unwrap(),
            "local.engine.game"
        );
        assert_eq!(
            bundle_identifier("x", Some("com.Studio-1.Game2")).unwrap(),
            "com.Studio-1.Game2"
        );
        for bad in [
            "",
            "single",
            ".a.b",
            "a..b",
            "a.b.",
            "a.-b",
            "a.b-",
            "a.b_c",
            "a.b c",
            "a.é",
            &format!("a.{}", "b".repeat(254)),
        ] {
            assert_eq!(
                code(bundle_identifier("x", Some(bad))),
                "tooling.app.bundle_id_invalid",
                "{bad:?}"
            );
        }
        // Every default is itself valid.
        for name in ["", "---", "A--B", "9 lives", &"z".repeat(300)] {
            validate_bundle_id(&bundle_identifier(name, None).unwrap()).unwrap();
        }
    }

    #[test]
    fn icns_has_the_header_one_ic08_entry_and_the_png_unchanged() {
        let png = default_icon().unwrap();
        let bytes = default_icns().unwrap();
        assert_eq!(&bytes[..4], b"icns");
        assert_eq!(
            u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize,
            bytes.len()
        );
        assert_eq!(&bytes[8..12], b"ic08");
        let entry = u32::from_be_bytes(bytes[12..16].try_into().unwrap()) as usize;
        assert_eq!(entry, png.len() + 8);
        assert_eq!(bytes.len(), 8 + entry);
        assert_eq!(&bytes[16..], &png[..]);
        assert_eq!(bytes, default_icns().unwrap());
        // A 256x256 PNG, as ic08 requires.
        let decoded = image::load_from_memory(&bytes[16..]).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (256, 256));
    }

    #[test]
    fn destination_must_end_in_app_and_name_something() {
        assert_eq!(
            bundle_file_stem(Path::new("dist/My Game.app")).unwrap(),
            "My Game"
        );
        assert_eq!(bundle_file_stem(Path::new("GAME.APP")).unwrap(), "GAME");
        for bad in [
            "game",
            "dist/game.zip",
            ".app",
            "..app",
            "...app",
            "a/.app",
            "game.app.bak",
        ] {
            assert_eq!(
                code(bundle_file_stem(Path::new(bad))),
                "tooling.destination.invalid",
                "{bad}"
            );
        }
    }

    #[test]
    fn resolve_checks_host_then_tools_then_identity() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("lipo"), "").unwrap();
        let path = std::env::join_paths([temp.path()]).unwrap();
        assert_eq!(
            code(MacosTooling::resolve(false, Some(path.as_os_str()), None)),
            "tooling.app.unsupported_host"
        );
        assert_eq!(
            code(MacosTooling::resolve(true, None, None)),
            "tooling.app.tool_missing"
        );
        // codesign is still missing.
        assert_eq!(
            code(MacosTooling::resolve(true, Some(path.as_os_str()), None)),
            "tooling.app.tool_missing"
        );
        fs::write(temp.path().join("codesign"), "").unwrap();
        // xattr is still missing.
        assert_eq!(
            code(MacosTooling::resolve(true, Some(path.as_os_str()), None)),
            "tooling.app.tool_missing"
        );
        fs::write(temp.path().join("xattr"), "").unwrap();
        let tooling = MacosTooling::resolve(true, Some(path.as_os_str()), None).unwrap();
        assert_eq!(tooling.lipo, temp.path().join("lipo"));
        assert_eq!(tooling.sign_identity, None);
        // An empty identity counts as unset.
        let tooling =
            MacosTooling::resolve(true, Some(path.as_os_str()), Some(OsStr::new(""))).unwrap();
        assert_eq!(tooling.sign_identity, None);
        let tooling = MacosTooling::resolve(
            true,
            Some(path.as_os_str()),
            Some(OsStr::new("Developer ID Application: A (TEAM)")),
        )
        .unwrap();
        assert_eq!(
            tooling.sign_identity.as_deref(),
            Some("Developer ID Application: A (TEAM)")
        );
    }

    #[test]
    fn codesign_arguments_are_ad_hoc_without_timestamp_or_identity_with_hardened_runtime() {
        let text = |identity| {
            codesign_arguments(identity)
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(text(None), ["--force", "--sign", "-", "--timestamp=none"]);
        assert_eq!(
            text(Some("ID")),
            [
                "--force",
                "--sign",
                "ID",
                "--options",
                "runtime",
                "--timestamp"
            ]
        );
        assert!(!text(None).contains(&"--deep".to_owned()));
    }
}
