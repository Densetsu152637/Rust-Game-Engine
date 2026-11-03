//! Additional read-only content roots: registration, confinement, links, kinds, limits,
//! replacement and the `@mount/id` namespace.
use super::common::*;
use engine_project::content_roots::{ContentRootEvent, ContentRoots, MAX_MOUNTS};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// A directory that plays the declared parent (Steam's `workshop/content/<app>`).
struct Mods {
    dir: tempfile::TempDir,
}

fn manifest(assets: Value) -> Value {
    json!({"schema": 1, "assets": assets})
}

impl Mods {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn parent(&self) -> &Path {
        self.dir.path()
    }
    fn path(&self, name: &str) -> PathBuf {
        self.parent().join(name)
    }
    /// A valid item with one asset of each allowed kind.
    fn good(&self, name: &str) -> PathBuf {
        let root = self.path(name);
        fs::create_dir_all(root.join("data")).unwrap();
        fs::write(root.join("data/white.ppm"), "P3\n1 1\n255\n255 255 255\n").unwrap();
        fs::write(root.join("data/fonttex.ppm"), ppm(8, 8, |_, _| [255; 3])).unwrap();
        fs::write(
            root.join("data/quad.mesh"),
            "CGMESH 1\nvertex 0 0 0 0 0\nvertex 1 0 0 1 0\nvertex 0 1 0 0 1\n",
        )
        .unwrap();
        fs::write(root.join("data/sfx.wav"), wav(8000, 1, 800)).unwrap();
        fs::write(
            root.join("data/font.json"),
            serde_json::to_vec(&font_json()).unwrap(),
        )
        .unwrap();
        self.manifest(
            name,
            json!([
                {"id": "tex", "kind": "texture", "path": "data/white.ppm"},
                {"id": "fonttex", "kind": "texture", "path": "data/fonttex.ppm"},
                {"id": "mesh", "kind": "mesh", "path": "data/quad.mesh"},
                {"id": "sfx", "kind": "audio", "path": "data/sfx.wav"},
                {"id": "font", "kind": "font", "path": "data/font.json"}
            ]),
        );
        root
    }
    fn manifest(&self, name: &str, assets: Value) {
        fs::write(
            self.path(name).join("content.json"),
            serde_json::to_vec(&manifest(assets)).unwrap(),
        )
        .unwrap();
    }
    fn register(
        &self,
        roots: &ContentRoots,
        mount: &str,
        name: &str,
    ) -> engine_project::Result<u64> {
        roots
            .register(mount, &self.path(name), self.parent())
            .map(|r| r.0)
    }
}

/// Creates a directory or file link; false when this OS/account cannot (reported by the
/// caller as a skip). On Windows a directory link falls back to a junction.
fn link(target: &Path, at: &Path, dir: bool) -> bool {
    #[cfg(unix)]
    {
        let _ = dir;
        std::os::unix::fs::symlink(target, at).is_ok()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs as wfs;
        let made = if dir {
            wfs::symlink_dir(target, at).is_ok()
        } else {
            wfs::symlink_file(target, at).is_ok()
        };
        if made {
            return true;
        }
        dir && Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(at)
            .arg(target)
            .output()
            .is_ok_and(|o| o.status.success())
    }
}

fn skipped(what: &str) {
    println!("skipped: this OS or account cannot create {what}");
}

#[test]
fn a_valid_root_registers_resolves_and_is_shared_by_project_clones() {
    let proj = Proj::new();
    let project = proj.project();
    let mods = Mods::new();
    mods.good("item1");
    let revision = mods
        .register(&project.content_roots, "workshop-1", "item1")
        .unwrap();
    let file = project.resolve_asset("@workshop-1/tex").unwrap();
    assert_eq!(
        file,
        fs::canonicalize(mods.path("item1/data/white.ppm")).unwrap()
    );
    // Clones (like the runtime's `ProjectResource`) see the same mounts.
    let clone = project.clone();
    assert!(clone.resolve_asset("@workshop-1/font").is_ok());
    let mounts = clone.content_roots.mounts();
    assert_eq!(mounts.len(), 1);
    assert_eq!(
        (mounts[0].mount.as_str(), mounts[0].assets),
        ("workshop-1", 5)
    );
    assert_eq!(mounts[0].revision.0, revision);
    assert_eq!(
        project
            .content_roots
            .asset("@workshop-1/mesh")
            .unwrap()
            .kind,
        "mesh"
    );
    assert_eq!(
        project.content_roots.drain_events(),
        vec![ContentRootEvent::Registered {
            mount: "workshop-1".into(),
            revision: mounts[0].revision
        }]
    );
    assert!(project.content_roots.drain_events().is_empty());
}

#[test]
fn project_ids_win_unknown_mod_ids_fail_and_at_is_reserved() {
    let proj = Proj::new();
    let project = proj.project();
    // The project's own ids resolve exactly as before, with or without mounted roots.
    let before = project.resolve_asset("mesh").unwrap();
    let mods = Mods::new();
    mods.good("item1");
    mods.register(&project.content_roots, "m", "item1").unwrap();
    assert_eq!(project.resolve_asset("mesh").unwrap(), before);
    for unknown in ["@nope/tex", "@m/ghost", "@m", "@/tex", "@m/"] {
        expect_code(project.resolve_asset(unknown), "project.asset.unknown");
    }
    // A project cannot declare an id in the reserved namespace.
    let mut p = Proj::new();
    p.add_asset("@evil/x", "mesh", "quad.mesh");
    expect_code(p.load(), "project.asset.id.reserved");
}

#[test]
fn mount_names_are_validated() {
    let mods = Mods::new();
    mods.good("item1");
    let roots = ContentRoots::default();
    for bad in ["", "a/b", "a b", "@x", &"x".repeat(65)] {
        expect_code(mods.register(&roots, bad, "item1"), "content_root.mount");
    }
    assert!(mods.register(&roots, &"x".repeat(64), "item1").is_ok());
    assert!(mods.register(&roots, "Mod_1.0-beta", "item1").is_ok());
}

#[test]
fn manifest_paths_cannot_escape_the_root() {
    let mods = Mods::new();
    let root = mods.good("item1");
    fs::write(mods.path("outside.ppm"), "P3\n1 1\n255\n0 0 0\n").unwrap();
    let roots = ContentRoots::default();
    let abs = fs::canonicalize(mods.path("outside.ppm")).unwrap();
    for path in [
        "../outside.ppm".to_owned(),
        "data/../../outside.ppm".to_owned(),
        abs.to_string_lossy().into_owned(),
        "".to_owned(),
        "missing.ppm".to_owned(),
    ] {
        mods.manifest(
            "item1",
            json!([{"id": "x", "kind": "texture", "path": path}]),
        );
        let result = mods.register(&roots, "m", "item1");
        assert!(result.is_err(), "{path:?} must be refused");
    }
    assert!(roots.mounts().is_empty());
    let _ = root;
}

#[test]
fn every_refused_kind_fails_the_whole_registration() {
    let mods = Mods::new();
    mods.good("item1");
    fs::write(mods.path("item1/x.dat"), "x").unwrap();
    let roots = ContentRoots::default();
    for kind in [
        "script",
        "text_script",
        "graph",
        "shader",
        "pcg",
        "gameplay",
        "font_face",
        "font_config",
        "locale_config",
        "video",
        "scene",
        "prefab",
        "model",
        "animation",
        "navigation",
        "acoustics",
        "partition",
        "tilemap",
        "tileset",
        "terrain",
        "foliage",
        "indirect",
        "caption_track",
        "nonsense",
    ] {
        mods.manifest(
            "item1",
            json!([
                {"id": "ok", "kind": "texture", "path": "data/white.ppm"},
                {"id": "bad", "kind": kind, "path": "x.dat"}
            ]),
        );
        expect_code(mods.register(&roots, "m", "item1"), "content_root.kind");
    }
    assert!(roots.mounts().is_empty(), "nothing is half-registered");
}

#[test]
fn duplicate_ids_paths_and_bad_manifests_are_refused() {
    let mods = Mods::new();
    mods.good("item1");
    let roots = ContentRoots::default();
    mods.manifest(
        "item1",
        json!([
            {"id": "a", "kind": "texture", "path": "data/white.ppm"},
            {"id": "a", "kind": "mesh", "path": "data/quad.mesh"}
        ]),
    );
    expect_code(mods.register(&roots, "m", "item1"), "content_root.asset.id");
    mods.manifest(
        "item1",
        json!([
            {"id": "a", "kind": "texture", "path": "data/white.ppm"},
            {"id": "b", "kind": "texture", "path": "data/WHITE.ppm"}
        ]),
    );
    expect_code(
        mods.register(&roots, "m", "item1"),
        "content_root.asset.path",
    );
    mods.manifest(
        "item1",
        json!([{"id": "", "kind": "texture", "path": "data/white.ppm"}]),
    );
    expect_code(mods.register(&roots, "m", "item1"), "content_root.asset.id");
    for text in [
        "{",
        r#"{"schema":2,"assets":[]}"#,
        r#"{"schema":1,"assets":[],"extra":1}"#,
    ] {
        fs::write(mods.path("item1/content.json"), text).unwrap();
        expect_code(mods.register(&roots, "m", "item1"), "content_root.manifest");
    }
    fs::remove_file(mods.path("item1/content.json")).unwrap();
    assert!(mods.register(&roots, "m", "item1").is_err());
    assert!(roots.mounts().is_empty());
}

#[test]
fn a_font_may_only_name_a_texture_of_its_own_root() {
    let mods = Mods::new();
    mods.good("item1");
    let roots = ContentRoots::default();
    // The font names "fonttex"; leave it out of the manifest.
    mods.manifest(
        "item1",
        json!([{"id": "font", "kind": "font", "path": "data/font.json"}]),
    );
    expect_code(mods.register(&roots, "m", "item1"), "content_root.font");
    // Naming a mesh (wrong kind) is refused too.
    mods.manifest(
        "item1",
        json!([
            {"id": "font", "kind": "font", "path": "data/font.json"},
            {"id": "fonttex", "kind": "mesh", "path": "data/quad.mesh"}
        ]),
    );
    expect_code(mods.register(&roots, "m", "item1"), "content_root.font");
}

#[test]
fn the_root_must_lie_strictly_below_its_declared_parent() {
    let mods = Mods::new();
    mods.good("item1");
    let other = Mods::new();
    other.good("elsewhere");
    let roots = ContentRoots::default();
    let escapes = |root: PathBuf, parent: &Path| {
        expect_code(roots.register("m", &root, parent), "content_root.escape");
    };
    // Outside the declared parent, equal to it, and reaching it through `..`.
    escapes(other.path("elsewhere"), mods.parent());
    escapes(mods.parent().to_owned(), mods.parent());
    escapes(mods.path("item1/../item1"), mods.parent());
    escapes(mods.path("item1"), &mods.path("item1"));
    // Relative paths are refused outright.
    expect_code(
        roots.register("m", Path::new("item1"), Path::new(".")),
        "content_root.path",
    );
    // A file instead of a directory.
    fs::write(mods.path("file"), "x").unwrap();
    assert!(
        roots
            .register("m", &mods.path("file"), mods.parent())
            .is_err()
    );
    assert!(roots.mounts().is_empty());
    assert!(
        roots
            .register("m", &mods.path("item1"), mods.parent())
            .is_ok()
    );
}

#[test]
fn a_link_as_the_root_or_between_parent_and_root_is_refused() {
    let mods = Mods::new();
    mods.good("real");
    let roots = ContentRoots::default();
    // The root itself is a link (symlink, or junction on Windows).
    if link(&mods.path("real"), &mods.path("linked"), true) {
        expect_code(mods.register(&roots, "m", "linked"), "content_root.link");
    } else {
        skipped("a directory link");
    }
    // A link between the declared parent and the root.
    fs::create_dir_all(mods.path("nest")).unwrap();
    mods.good("nest/inner");
    if link(&mods.path("nest"), &mods.path("nestlink"), true) {
        let result = roots.register("m", &mods.path("nestlink/inner"), mods.parent());
        expect_code(result, "content_root.link");
    } else {
        skipped("a directory link");
    }
    assert!(roots.mounts().is_empty());
}

#[test]
fn links_inside_a_root_are_refused_even_when_they_stay_inside() {
    let mods = Mods::new();
    let root = mods.good("item1");
    let roots = ContentRoots::default();
    // A file link to a file inside the root.
    fs::write(root.join("real.ppm"), "P3\n1 1\n255\n0 0 0\n").unwrap();
    if link(&root.join("real.ppm"), &root.join("alias.ppm"), false) {
        mods.manifest(
            "item1",
            json!([{"id": "a", "kind": "texture", "path": "alias.ppm"}]),
        );
        expect_code(mods.register(&roots, "m", "item1"), "content_root.link");
    } else {
        skipped("a file symlink");
    }
    // A directory link inside the root, used as a path component.
    if link(&root.join("data"), &root.join("dirlink"), true) {
        mods.manifest(
            "item1",
            json!([{"id": "a", "kind": "texture", "path": "dirlink/white.ppm"}]),
        );
        expect_code(mods.register(&roots, "m", "item1"), "content_root.link");
    } else {
        skipped("a directory link");
    }
    // A link pointing outside the root fails the usual confinement as well.
    fs::write(mods.path("outside.ppm"), "P3\n1 1\n255\n0 0 0\n").unwrap();
    if link(&mods.path("outside.ppm"), &root.join("out.ppm"), false) {
        mods.manifest(
            "item1",
            json!([{"id": "a", "kind": "texture", "path": "out.ppm"}]),
        );
        assert!(mods.register(&roots, "m", "item1").is_err());
    } else {
        skipped("a file symlink");
    }
    // The manifest itself may not be a link.
    mods.manifest(
        "item1",
        json!([{"id": "a", "kind": "texture", "path": "real.ppm"}]),
    );
    fs::rename(root.join("content.json"), root.join("real.json")).unwrap();
    if link(&root.join("real.json"), &root.join("content.json"), false) {
        expect_code(mods.register(&roots, "m", "item1"), "content_root.link");
    } else {
        skipped("a file symlink");
    }
    assert!(roots.mounts().is_empty());
}

#[test]
fn a_link_swapped_in_after_registration_fails_the_next_resolution() {
    let mods = Mods::new();
    let root = mods.good("item1");
    let roots = ContentRoots::default();
    mods.register(&roots, "m", "item1").unwrap();
    assert!(roots.resolve("@m/tex").is_ok());
    // Replace a registered file with a link to another file inside the root.
    fs::write(root.join("other.ppm"), "P3\n1 1\n255\n0 0 0\n").unwrap();
    fs::remove_file(root.join("data/white.ppm")).unwrap();
    if link(&root.join("other.ppm"), &root.join("data/white.ppm"), false) {
        expect_code(roots.resolve("@m/tex"), "content_root.link");
    } else {
        skipped("a file symlink");
        expect_code(roots.resolve("@m/tex"), "project.asset.missing");
    }
    // The root itself replaced by a link (directory moved away, link put in its place).
    fs::rename(&root, mods.path("moved")).unwrap();
    if link(&mods.path("moved"), &root, true) {
        expect_code(roots.resolve("@m/mesh"), "content_root.link");
    } else {
        skipped("a directory link");
    }
}

#[test]
fn replacement_is_atomic_and_bumps_the_revision() {
    let mods = Mods::new();
    let root = mods.good("item1");
    let roots = ContentRoots::default();
    let first = mods.register(&roots, "m", "item1").unwrap();
    // A broken update keeps the previous registration.
    fs::write(root.join("content.json"), "{").unwrap();
    assert!(mods.register(&roots, "m", "item1").is_err());
    assert_eq!(roots.mounts()[0].revision.0, first);
    assert!(roots.resolve("@m/tex").is_ok());
    // A good update swaps assets and bumps the revision.
    mods.manifest(
        "item1",
        json!([{"id": "only", "kind": "mesh", "path": "data/quad.mesh"}]),
    );
    let second = mods.register(&roots, "m", "item1").unwrap();
    assert!(second > first);
    expect_code(roots.resolve("@m/tex"), "project.asset.unknown");
    assert!(roots.resolve("@m/only").is_ok());
    let events = roots.drain_events();
    assert!(matches!(events[0], ContentRootEvent::Registered { .. }));
    assert!(matches!(events[1], ContentRootEvent::Replaced { .. }));
    assert_eq!(events.len(), 2);
}

#[test]
fn unregister_makes_ids_unknown_and_reports_an_event() {
    let mods = Mods::new();
    mods.good("item1");
    let roots = ContentRoots::default();
    mods.register(&roots, "m", "item1").unwrap();
    roots.drain_events();
    assert!(roots.unregister("m"));
    assert!(!roots.unregister("m"));
    expect_code(roots.resolve("@m/tex"), "project.asset.unknown");
    assert_eq!(
        roots.drain_events(),
        vec![ContentRootEvent::Unregistered { mount: "m".into() }]
    );
    // Removing the directory while mounted is a plain missing-file error, not a panic.
    mods.register(&roots, "m", "item1").unwrap();
    fs::remove_dir_all(mods.path("item1")).unwrap();
    assert!(roots.resolve("@m/tex").is_err());
}

#[test]
fn limits_bound_mounts_and_assets() {
    let mods = Mods::new();
    mods.good("item1");
    let roots = ContentRoots::default();
    for n in 0..MAX_MOUNTS {
        mods.register(&roots, &format!("m{n}"), "item1").unwrap();
    }
    expect_code(
        mods.register(&roots, "extra", "item1"),
        "content_root.limit",
    );
    // Replacing an existing mount at the cap is fine.
    assert!(mods.register(&roots, "m0", "item1").is_ok());
    assert_eq!(roots.mounts().len(), MAX_MOUNTS);

    // More assets than one root may declare.
    let many: Vec<Value> = (0..4097)
        .map(|n| json!({"id": format!("a{n}"), "kind": "texture", "path": "data/white.ppm"}))
        .collect();
    mods.manifest("item1", Value::Array(many));
    let fresh = ContentRoots::default();
    expect_code(mods.register(&fresh, "m", "item1"), "content_root.limit");

    // A manifest over 1 MiB.
    let padding = " ".repeat(1024 * 1024 + 1);
    fs::write(
        mods.path("item1/content.json"),
        format!(r#"{{"schema":1,"assets":[]}}{padding}"#),
    )
    .unwrap();
    expect_code(
        mods.register(&fresh, "m", "item1"),
        "project.file.too_large",
    );
}

#[test]
fn the_total_asset_budget_spans_all_roots() {
    let mods = Mods::new();
    let roots = ContentRoots::default();
    // Four full roots of 4096 assets reach the 16384 total; one more asset is refused.
    let make = |name: &str, count: usize| {
        let dir = mods.path(name);
        fs::create_dir_all(dir.join("f")).unwrap();
        let assets: Vec<Value> = (0..count)
            .map(|n| {
                let file = format!("f/{n}.ppm");
                fs::write(dir.join(&file), "P3\n1 1\n255\n0 0 0\n").unwrap();
                json!({"id": format!("a{n}"), "kind": "texture", "path": file})
            })
            .collect();
        mods.manifest(name, Value::Array(assets));
    };
    for n in 0..4 {
        make(&format!("big{n}"), 4096);
        mods.register(&roots, &format!("b{n}"), &format!("big{n}"))
            .unwrap();
    }
    make("one", 1);
    expect_code(mods.register(&roots, "one", "one"), "content_root.limit");
    // Replacing a full mount with a smaller root frees room.
    mods.register(&roots, "b0", "one").unwrap();
    assert!(mods.register(&roots, "one", "one").is_ok());
}
