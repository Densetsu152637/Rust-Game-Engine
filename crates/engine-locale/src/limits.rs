//! Bounds applied to untrusted resources, keys and output.

/// Resource and output bounds. All checks return typed errors, never panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleLimits {
    /// Maximum bytes in one `.ftl` source.
    pub max_resource_bytes: usize,
    /// Maximum messages plus terms per locale (across all loaded sources).
    pub max_messages_per_locale: usize,
    /// Maximum loaded locales.
    pub max_locales: usize,
    /// Maximum nesting of placeables and call arguments in a source.
    pub max_placeable_depth: usize,
    /// Maximum bytes in a lookup key.
    pub max_key_bytes: usize,
    /// Maximum bytes of one formatted message.
    pub max_output_bytes: usize,
    /// Maximum bytes of one string argument; longer values are truncated at a char boundary.
    pub max_arg_bytes: usize,
    /// Maximum distinct diagnostics retained (further ones are counted in `dropped`).
    pub max_diagnostics: usize,
}

impl Default for LocaleLimits {
    fn default() -> Self {
        Self {
            max_resource_bytes: 1 << 20,
            max_messages_per_locale: 10_000,
            max_locales: 64,
            max_placeable_depth: 8,
            max_key_bytes: 256,
            max_output_bytes: 64 << 10,
            max_arg_bytes: 16 << 10,
            max_diagnostics: 256,
        }
    }
}
