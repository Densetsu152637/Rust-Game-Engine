//! Synthetic local executables shared by compiler lifecycle regressions.
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) fn build_hanging_probe(root: &Path) -> PathBuf {
    let source = root.join("hanging_probe.rs");
    let executable = root.join(if cfg!(windows) {
        "hanging_probe.exe"
    } else {
        "hanging_probe"
    });
    std::fs::write(&source, r#"fn main() {
        let exe=std::env::current_exe().unwrap();
        if std::env::args().any(|a|a=="--child") {
            std::fs::write(exe.with_extension("child"),b"started").unwrap();
        } else {
            std::process::Command::new(&exe).arg("--child").stdin(std::process::Stdio::null()).spawn().unwrap();
            std::fs::write(exe.with_extension("parent"),b"started").unwrap();
        }
        std::thread::sleep(std::time::Duration::from_secs(60));
    }"#).unwrap();
    let cargo = Path::new(env!("CARGO"));
    let rustc = cargo
        .parent()
        .unwrap()
        .join(if cfg!(windows) { "rustc.exe" } else { "rustc" });
    assert!(
        Command::new(rustc)
            .arg(source)
            .arg("-o")
            .arg(&executable)
            .status()
            .unwrap()
            .success()
    );
    executable
}
