use anyhow::{Result, ensure};

/// Parse and validate a standalone native WGSL shader without opening a device.
/// All native capabilities are admitted here; device feature/limit requirements
/// are checked separately when the actual pipeline is created. Diagnostics retain
/// Naga source spans. Sources must contain an entry point and fit within 16 MiB.
pub fn validate_wgsl(source: &str) -> Result<()> {
    ensure!(
        source.len() <= 16 * 1024 * 1024,
        "WGSL source exceeds 16 MiB"
    );
    let module = naga::front::wgsl::parse_str(source)
        .map_err(|error| anyhow::anyhow!("WGSL parse failed:\n{}", error.emit_to_string(source)))?;
    ensure!(
        !module.entry_points.is_empty(),
        "WGSL shader has no entry point"
    );
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .map_err(|error| {
        anyhow::anyhow!("WGSL validation failed:\n{}", error.emit_to_string(source))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_real_shader_and_reports_parse_and_semantic_errors_without_gpu() {
        validate_wgsl(include_str!("textured.wgsl")).unwrap();
        assert!(
            validate_wgsl("@vertex fn broken(")
                .unwrap_err()
                .to_string()
                .contains("WGSL parse failed")
        );
        assert!(
            validate_wgsl("@vertex fn main() -> @builtin(position) f32 { return 1.0; }")
                .unwrap_err()
                .to_string()
                .contains("WGSL validation failed")
        );
        assert!(validate_wgsl("").is_err());
    }
}
