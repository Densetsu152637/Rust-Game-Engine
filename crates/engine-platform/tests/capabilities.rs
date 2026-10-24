use engine_platform::{Capability, HostPlatform, PackageHost};

#[test]
fn desktop_hosts_expose_implemented_paths_without_promising_devices() {
    for platform in [
        HostPlatform::Windows,
        HostPlatform::Linux,
        HostPlatform::Macos,
    ] {
        for entry in platform.capabilities().capabilities {
            platform.require(entry.capability).unwrap();
            assert!(entry.implemented);
            assert!(!entry.limitation.is_empty());
        }
    }
}

#[test]
fn unimplemented_hosts_fail_with_actionable_reasons() {
    for platform in [
        HostPlatform::Android,
        HostPlatform::Ios,
        HostPlatform::Web,
        HostPlatform::Unknown,
    ] {
        for capability in Capability::ALL {
            let error = platform.require(capability).unwrap_err();
            assert_eq!(error.platform, platform);
            assert_eq!(error.capability, capability);
            assert!(error.to_string().contains(platform.name()));
            assert!(!error.reason.is_empty());
        }
    }
}

#[test]
fn platform_names_round_trip_and_reject_unknown_names() {
    for platform in HostPlatform::ALL {
        assert_eq!(platform.name().parse::<HostPlatform>().unwrap(), platform);
    }
    for invalid in ["", "win", "Windows", "ps5", "linux-x86_64"] {
        assert!(invalid.parse::<HostPlatform>().is_err());
    }
}

#[test]
fn package_metadata_records_the_actual_host_and_native_launcher() {
    let host = PackageHost::current().unwrap();
    assert_eq!(host.platform, HostPlatform::current());
    assert_eq!(host.architecture, std::env::consts::ARCH);
    if cfg!(windows) {
        assert_eq!(
            (&*host.runtime_filename, &*host.launcher_filename),
            ("runtime.exe", "run.cmd")
        );
    } else {
        assert_eq!(
            (&*host.runtime_filename, &*host.launcher_filename),
            ("runtime", "run.sh")
        );
    }
}
