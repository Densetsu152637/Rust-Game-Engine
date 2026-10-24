use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(ngx_available)");
    println!("cargo:rerun-if-env-changed=NGX_SDK_DIR");
    println!("cargo:rerun-if-env-changed=VULKAN_HEADERS_DIR");
    println!("cargo:rerun-if-changed=native/ngx.cpp");
    println!("cargo:rerun-if-changed=native/ngx_dlssg.cpp");
    println!("cargo:rerun-if-changed=native/ngx_bridge.h");
    if env::var_os("CARGO_FEATURE_NGX_VULKAN").is_none() {
        return;
    }
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if !matches!(os.as_str(), "windows" | "linux") || !matches!(arch.as_str(), "x86_64" | "aarch64")
    {
        return;
    }
    let Some(sdk) = env::var_os("NGX_SDK_DIR").map(PathBuf::from) else {
        return;
    };
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && target_env != "msvc" {
        // The official Windows loader is an MSVC static library.
        println!(
            "cargo:warning=engine-dlss: NGX needs the MSVC toolchain on Windows (target env `{target_env}`); building the unavailable adapter"
        );
        return;
    }
    let headers = env::var_os("VULKAN_HEADERS_DIR")
        .map(PathBuf::from)
        .expect("VULKAN_HEADERS_DIR must name the official Vulkan-Headers include directory");
    assert!(
        headers.join("vulkan").join("vulkan.h").is_file(),
        "VULKAN_HEADERS_DIR must name the official Vulkan-Headers include directory (missing vulkan/vulkan.h)"
    );
    let include = sdk.join("include");
    assert!(
        include.join("nvsdk_ngx_helpers_dlssg_vk.h").is_file(),
        "NGX_SDK_DIR does not contain the official NGX headers (include/nvsdk_ngx_helpers_dlssg_vk.h)"
    );
    println!("cargo:rerun-if-changed={}", include.display());
    println!("cargo:rerun-if-changed={}", headers.display());
    let platform = if os == "windows" { "Windows" } else { "Linux" };
    let arch_dir = if arch == "aarch64" {
        "aarch64"
    } else {
        "x86_64"
    };
    let mut library = sdk.join("lib").join(format!("{platform}_{arch_dir}"));
    if os == "windows" && arch == "x86_64" {
        library.push("x64");
    }
    let name = if os == "windows" {
        "nvsdk_ngx_d"
    } else {
        "nvsdk_ngx"
    };
    let filename = if os == "windows" {
        format!("{name}.lib")
    } else {
        format!("lib{name}.a")
    };
    assert!(
        library.join(&filename).is_file(),
        "NGX_SDK_DIR does not contain the official NGX loader library"
    );
    println!(
        "cargo:rerun-if-changed={}",
        library.join(&filename).display()
    );
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .include(&include)
        .include(headers)
        .file("native/ngx.cpp")
        .file("native/ngx_dlssg.cpp")
        .compile("engine_ngx_bridge");
    println!("cargo:rustc-link-search=native={}", library.display());
    println!("cargo:rustc-link-lib=static={name}");
    if os == "windows" {
        for lib in [
            "advapi32", "shlwapi", "version", "ole32", "shell32", "user32",
        ] {
            println!("cargo:rustc-link-lib={lib}");
        }
    } else {
        println!("cargo:rustc-link-lib=dl");
    }
    println!("cargo:rustc-cfg=ngx_available");
}
