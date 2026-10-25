//! Render feature registration: stable names, named stages, before/after
//! constraints, deterministic order and per-view enable state.

use crate::context::{FeatureContext, FeatureSetup};
use anyhow::{Result, bail, ensure};
use std::collections::{BTreeMap, BTreeSet};

/// Longest feature name, in bytes.
pub const MAX_FEATURE_NAME_BYTES: usize = 64;
/// Most features one registry holds.
pub const MAX_FEATURES: usize = 256;
/// Most ordering constraints per feature.
pub const MAX_CONSTRAINTS: usize = 16;

/// Where in the frame a feature runs. Stages execute in declaration order;
/// within a stage features run in [`FeatureRegistry::order`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    /// CPU preparation, uploads and compute dispatches (GPU culling,
    /// skinning, particle simulation). No render target is bound.
    Prepare,
    /// Shadow map rendering into the view's shadow atlas.
    Shadow,
    /// Depth/normal/motion prepasses at the render size.
    Prepass,
    /// Opaque geometry into HDR colour + depth (render size).
    Opaque,
    /// Sky and background, after opaque geometry (depth-tested at far depth).
    Sky,
    /// Blended geometry, sorted by the feature.
    Transparent,
    /// Render size to output size: temporal resolve/upscalers. The last
    /// enabled feature of this stage runs (see [`StageRule::LastEnabledWins`]).
    Upscale,
    /// HDR post-processing at the output size (ping-pong colour targets).
    PostProcess,
    /// The output operator: HDR to the target format. The last enabled
    /// feature of this stage runs; `engine.output` is the default.
    Tonemap,
    /// After tonemapping, in the target format: UI, gizmos, debug draw.
    Overlay,
}

impl Stage {
    pub const ALL: [Stage; 10] = [
        Self::Prepare,
        Self::Shadow,
        Self::Prepass,
        Self::Opaque,
        Self::Sky,
        Self::Transparent,
        Self::Upscale,
        Self::PostProcess,
        Self::Tonemap,
        Self::Overlay,
    ];

    /// Stable lowercase name used in status reports.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Shadow => "shadow",
            Self::Prepass => "prepass",
            Self::Opaque => "opaque",
            Self::Sky => "sky",
            Self::Transparent => "transparent",
            Self::Upscale => "upscale",
            Self::PostProcess => "post_process",
            Self::Tonemap => "tonemap",
            Self::Overlay => "overlay",
        }
    }

    /// How a stage treats several enabled features.
    pub const fn rule(self) -> StageRule {
        match self {
            Self::Upscale | Self::Tonemap => StageRule::LastEnabledWins,
            _ => StageRule::All,
        }
    }
}

/// How many enabled features of a stage run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageRule {
    /// Every enabled feature runs in order.
    All,
    /// Only the last enabled feature in order runs; the others are reported
    /// as superseded. Lets a tonemapper or upscaler replace the default.
    LastEnabledWins,
}

/// Optional inputs a feature needs; the pipeline produces them for a view
/// only when an enabled feature (or the host) requests them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeatureNeeds {
    /// World-space normals from the `engine.prepass.normals` prepass.
    pub normals: bool,
    /// Current-to-previous motion vectors from `engine.prepass.motion`.
    pub motion: bool,
}

impl FeatureNeeds {
    pub fn union(self, other: Self) -> Self {
        Self {
            normals: self.normals || other.normals,
            motion: self.motion || other.motion,
        }
    }
}

/// Registration data of one feature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureDescriptor {
    /// Stable unique name: 1..=64 bytes of `[a-z0-9._-]`, conventionally
    /// `<crate-or-domain>.<feature>` (`engine.` is reserved for built-ins).
    pub name: String,
    pub stage: Stage,
    /// Features (same stage) this one must run before. Names that are not
    /// registered are ignored, so optional neighbours can be named.
    pub before: Vec<String>,
    /// Features (same stage) this one must run after.
    pub after: Vec<String>,
    /// State in views without an override.
    pub enabled_by_default: bool,
    /// Built-ins the host cannot run without (`engine.output`); per-view
    /// overrides cannot disable them.
    pub required: bool,
    pub needs: FeatureNeeds,
}

impl FeatureDescriptor {
    pub fn new(name: impl Into<String>, stage: Stage) -> Self {
        Self {
            name: name.into(),
            stage,
            before: Vec::new(),
            after: Vec::new(),
            enabled_by_default: true,
            required: false,
            needs: FeatureNeeds::default(),
        }
    }

    pub fn before(mut self, name: impl Into<String>) -> Self {
        self.before.push(name.into());
        self
    }

    pub fn after(mut self, name: impl Into<String>) -> Self {
        self.after.push(name.into());
        self
    }

    pub fn disabled_by_default(mut self) -> Self {
        self.enabled_by_default = false;
        self
    }

    pub fn needs(mut self, needs: FeatureNeeds) -> Self {
        self.needs = needs;
        self
    }

    pub(crate) fn required(mut self) -> Self {
        self.required = true;
        self
    }
}

/// Whether `name` is a valid feature name.
pub fn validate_feature_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= MAX_FEATURE_NAME_BYTES,
        "render feature names must be 1..={MAX_FEATURE_NAME_BYTES} bytes: {name:?}"
    );
    ensure!(
        name.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)),
        "render feature names use only [a-z0-9._-]: {name:?}"
    );
    Ok(())
}

/// A render feature: a pass (or group of passes) in one [`Stage`].
///
/// `prepare` runs once per device before the first frame (create pipelines
/// against [`FeatureSetup::view_layout`]); `encode` runs each frame for every
/// view that has the feature enabled, in stage order. Implementations own
/// their GPU objects. Errors stop the frame and name the feature.
pub trait PipelineFeature: Send + Sync + 'static {
    fn descriptor(&self) -> FeatureDescriptor;

    fn prepare(&mut self, _setup: &FeatureSetup<'_>) -> Result<()> {
        Ok(())
    }

    fn encode(&mut self, ctx: &mut FeatureContext<'_>) -> Result<()>;
}

/// Where a registered feature's work comes from.
pub(crate) enum FeatureBody {
    /// Owned implementation run by the pipeline.
    Owned(Box<dyn PipelineFeature>),
    /// A node the host executes itself (built-in `engine.sprites` overlay).
    Host,
    /// Built-in work the pipeline performs inline (mesh passes, output).
    Builtin,
    /// A descriptor without an implementation (status previews only).
    Descriptor,
}

pub(crate) struct Registered {
    pub descriptor: FeatureDescriptor,
    pub body: FeatureBody,
    pub prepared: bool,
}

/// The registered features of a pipeline, in deterministic order.
///
/// Order: stages in [`Stage::ALL`] order; within a stage a topological sort
/// of the before/after constraints, ties broken by registration order. A
/// cycle or a constraint naming a feature of another stage is an error at
/// registration, leaving the registry unchanged.
#[derive(Default)]
pub struct FeatureRegistry {
    pub(crate) features: Vec<Registered>,
    order: Vec<usize>,
}

impl FeatureRegistry {
    /// Register an owned feature.
    pub fn register(&mut self, feature: impl PipelineFeature) -> Result<()> {
        self.register_boxed(Box::new(feature))
    }

    pub fn register_boxed(&mut self, feature: Box<dyn PipelineFeature>) -> Result<()> {
        let descriptor = feature.descriptor();
        ensure!(
            !descriptor.name.starts_with("engine."),
            "render feature names starting with `engine.` are reserved: {}",
            descriptor.name
        );
        self.insert(descriptor, FeatureBody::Owned(feature))
    }

    pub(crate) fn insert(
        &mut self,
        descriptor: FeatureDescriptor,
        body: FeatureBody,
    ) -> Result<()> {
        validate_feature_name(&descriptor.name)?;
        ensure!(
            self.features.len() < MAX_FEATURES,
            "render feature limit of {MAX_FEATURES} exceeded"
        );
        ensure!(
            !self
                .features
                .iter()
                .any(|f| f.descriptor.name == descriptor.name),
            "duplicate render feature {}",
            descriptor.name
        );
        ensure!(
            descriptor.before.len() + descriptor.after.len() <= MAX_CONSTRAINTS,
            "render feature {} has more than {MAX_CONSTRAINTS} ordering constraints",
            descriptor.name
        );
        for name in descriptor.before.iter().chain(&descriptor.after) {
            validate_feature_name(name)?;
            ensure!(
                *name != descriptor.name,
                "render feature {} cannot be ordered relative to itself",
                descriptor.name
            );
        }
        let mut descriptors: Vec<&FeatureDescriptor> =
            self.features.iter().map(|f| &f.descriptor).collect();
        descriptors.push(&descriptor);
        let order = compute_order(&descriptors)?;
        self.features.push(Registered {
            descriptor,
            body,
            prepared: false,
        });
        self.order = order;
        Ok(())
    }

    /// Indices in execution order.
    pub(crate) fn ordered(&self) -> &[usize] {
        &self.order
    }

    /// Descriptors in execution order.
    pub fn descriptors(&self) -> impl Iterator<Item = &FeatureDescriptor> {
        self.order.iter().map(|&i| &self.features[i].descriptor)
    }

    /// Names in execution order.
    pub fn names(&self) -> Vec<&str> {
        self.descriptors().map(|d| d.name.as_str()).collect()
    }

    pub fn len(&self) -> usize {
        self.features.len()
    }

    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    pub fn descriptor(&self, name: &str) -> Option<&FeatureDescriptor> {
        self.features
            .iter()
            .map(|f| &f.descriptor)
            .find(|d| d.name == name)
    }

    /// Move every owned feature of `other` into this registry (in `other`'s
    /// registration order). Fails without changes on the first conflict.
    pub fn absorb(&mut self, other: FeatureRegistry) -> Result<()> {
        let mut candidate: Vec<&FeatureDescriptor> =
            self.features.iter().map(|f| &f.descriptor).collect();
        for feature in &other.features {
            ensure!(
                !candidate.iter().any(|d| d.name == feature.descriptor.name),
                "duplicate render feature {}",
                feature.descriptor.name
            );
            candidate.push(&feature.descriptor);
        }
        ensure!(
            candidate.len() <= MAX_FEATURES,
            "render feature limit of {MAX_FEATURES} exceeded"
        );
        let order = compute_order(&candidate)?;
        self.features.extend(other.features);
        self.order = order;
        Ok(())
    }

    /// The same descriptors without implementations, for status previews
    /// (resolving per-view state without a device).
    pub fn descriptor_copy(&self) -> Result<FeatureRegistry> {
        let mut copy = FeatureRegistry::default();
        for &index in &self.order {
            copy.insert(
                self.features[index].descriptor.clone(),
                FeatureBody::Descriptor,
            )?;
        }
        Ok(copy)
    }

    /// Resolve which features are enabled for one view.
    pub fn resolve(&self, overrides: &FeatureOverrides) -> ResolvedFeatures {
        let mut states = Vec::with_capacity(self.features.len());
        for &index in &self.order {
            let descriptor = &self.features[index].descriptor;
            let override_value = overrides.get(&descriptor.name);
            let (enabled, source) = match (descriptor.required, override_value) {
                (true, Some(false)) => (true, EnableSource::RequiredOverrideIgnored),
                (true, _) => (true, EnableSource::Required),
                (false, Some(value)) => (value, EnableSource::ViewOverride),
                (false, None) => (descriptor.enabled_by_default, EnableSource::Default),
            };
            states.push(FeatureState {
                index,
                name: descriptor.name.clone(),
                stage: descriptor.stage,
                enabled,
                source,
                activity: if enabled {
                    Activity::Pending
                } else {
                    Activity::Disabled
                },
            });
        }
        // Single-winner stages: only the last enabled feature runs.
        for stage in Stage::ALL
            .into_iter()
            .filter(|s| s.rule() == StageRule::LastEnabledWins)
        {
            let winner = states
                .iter()
                .rposition(|s| s.stage == stage && s.enabled)
                .map(|i| states[i].name.clone());
            for state in states.iter_mut().filter(|s| s.stage == stage && s.enabled) {
                if Some(&state.name) != winner.as_ref() {
                    state.activity = Activity::Superseded(winner.clone().unwrap_or_default());
                }
            }
        }
        let unknown = overrides
            .iter()
            .filter(|(name, _)| self.descriptor(name).is_none())
            .map(|(name, _)| name.to_owned())
            .collect();
        ResolvedFeatures { states, unknown }
    }

    pub(crate) fn needs(&self, resolved: &ResolvedFeatures) -> FeatureNeeds {
        resolved
            .states
            .iter()
            .filter(|s| s.runs())
            .fold(FeatureNeeds::default(), |needs, s| {
                needs.union(self.features[s.index].descriptor.needs)
            })
    }
}

/// Features registered by game code or other crates before the host creates
/// its renderer (a world resource the runner hands to the renderer).
#[derive(bevy_ecs::prelude::Resource, Default)]
pub struct PipelineFeatures(pub FeatureRegistry);

impl PipelineFeatures {
    pub fn register(&mut self, feature: impl PipelineFeature) -> Result<()> {
        self.0.register(feature)
    }
}

/// Per-view enable overrides by feature name (authored on `Camera3D` as
/// `render_features`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeatureOverrides(pub BTreeMap<String, bool>);

impl FeatureOverrides {
    pub fn get(&self, name: &str) -> Option<bool> {
        self.0.get(name).copied()
    }

    pub fn set(&mut self, name: impl Into<String>, enabled: bool) {
        self.0.insert(name.into(), enabled);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, bool)> {
        self.0.iter().map(|(k, v)| (k.as_str(), *v))
    }
}

/// Why a feature is enabled or disabled in a view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnableSource {
    Default,
    ViewOverride,
    Required,
    /// The view tried to disable a required feature; it stays enabled.
    RequiredOverrideIgnored,
}

impl EnableSource {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::ViewOverride => "view_override",
            Self::Required => "required",
            Self::RequiredOverrideIgnored => "required_override_ignored",
        }
    }
}

/// What a feature did in the last frame of a view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activity {
    /// Enabled; not executed yet this frame.
    Pending,
    /// Encoded work this frame.
    Ran,
    /// Enabled, but nothing to do this frame (for example a prepass nobody
    /// needed, or an upscaler with equal render and output sizes).
    Idle,
    Disabled,
    /// Enabled in a single-winner stage, but the named feature ran instead.
    Superseded(String),
    /// Enabled, but the host replaced the stage (for example DLSS upscaling).
    ReplacedByHost,
}

impl Activity {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ran => "ran",
            Self::Idle => "idle",
            Self::Disabled => "disabled",
            Self::Superseded(_) => "superseded",
            Self::ReplacedByHost => "replaced_by_host",
        }
    }
}

/// One feature's state in one view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureState {
    pub(crate) index: usize,
    pub name: String,
    pub stage: Stage,
    pub enabled: bool,
    pub source: EnableSource,
    pub activity: Activity,
}

impl FeatureState {
    /// Enabled and not superseded.
    pub fn runs(&self) -> bool {
        self.enabled && !matches!(self.activity, Activity::Superseded(_))
    }
}

/// The resolved feature list of one view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedFeatures {
    /// In execution order.
    pub states: Vec<FeatureState>,
    /// Override names that match no registered feature.
    pub unknown: Vec<String>,
}

impl ResolvedFeatures {
    pub fn get(&self, name: &str) -> Option<&FeatureState> {
        self.states.iter().find(|s| s.name == name)
    }

    pub fn is_enabled(&self, name: &str) -> bool {
        self.get(name).is_some_and(FeatureState::runs)
    }

    pub(crate) fn set_activity(&mut self, name: &str, activity: Activity) {
        if let Some(state) = self.states.iter_mut().find(|s| s.name == name) {
            state.activity = activity;
        }
    }
}

/// Deterministic execution order (indices into `descriptors`).
fn compute_order(descriptors: &[&FeatureDescriptor]) -> Result<Vec<usize>> {
    let index_of: BTreeMap<&str, usize> = descriptors
        .iter()
        .enumerate()
        .map(|(i, d)| (d.name.as_str(), i))
        .collect();
    // Edges a -> b: a runs before b.
    let mut edges: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); descriptors.len()];
    for (i, descriptor) in descriptors.iter().enumerate() {
        let mut link = |from: usize, to: usize, other: &str| -> Result<()> {
            let (a, b) = (descriptors[from], descriptors[to]);
            if a.stage != b.stage {
                bail!(
                    "render feature {} orders itself against {other} of stage {}, but it runs in \
                     stage {}; constraints apply within one stage",
                    descriptor.name,
                    descriptors[index_of[other]].stage.name(),
                    descriptor.stage.name()
                );
            }
            edges[from].insert(to);
            Ok(())
        };
        for name in &descriptor.before {
            if let Some(&j) = index_of.get(name.as_str()) {
                link(i, j, name)?;
            }
        }
        for name in &descriptor.after {
            if let Some(&j) = index_of.get(name.as_str()) {
                link(j, i, name)?;
            }
        }
    }
    let mut incoming = vec![0usize; descriptors.len()];
    for targets in &edges {
        for &t in targets {
            incoming[t] += 1;
        }
    }
    let mut order = Vec::with_capacity(descriptors.len());
    for stage in Stage::ALL {
        let members: Vec<usize> = (0..descriptors.len())
            .filter(|&i| descriptors[i].stage == stage)
            .collect();
        let mut ready: BTreeSet<usize> = members
            .iter()
            .copied()
            .filter(|&i| incoming[i] == 0)
            .collect();
        let mut placed = 0;
        // Lowest registration index first among the ready set.
        while let Some(next) = ready.pop_first() {
            order.push(next);
            placed += 1;
            for &t in &edges[next] {
                incoming[t] -= 1;
                if incoming[t] == 0 {
                    ready.insert(t);
                }
            }
        }
        if placed != members.len() {
            let cycle: Vec<&str> = members
                .iter()
                .filter(|&&i| incoming[i] > 0)
                .map(|&i| descriptors[i].name.as_str())
                .collect();
            bail!(
                "render feature ordering cycle in stage {}: {}",
                stage.name(),
                cycle.join(", ")
            );
        }
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub(FeatureDescriptor);
    impl PipelineFeature for Stub {
        fn descriptor(&self) -> FeatureDescriptor {
            self.0.clone()
        }
        fn encode(&mut self, _: &mut FeatureContext<'_>) -> Result<()> {
            Ok(())
        }
    }

    fn feature(name: &str, stage: Stage) -> FeatureDescriptor {
        FeatureDescriptor::new(name, stage)
    }

    fn build(features: impl IntoIterator<Item = FeatureDescriptor>) -> FeatureRegistry {
        let mut registry = FeatureRegistry::default();
        for descriptor in features {
            registry.register(Stub(descriptor)).unwrap();
        }
        registry
    }

    #[test]
    fn stages_order_first_then_registration_order() {
        let registry = build([
            feature("a.overlay", Stage::Overlay),
            feature("a.post", Stage::PostProcess),
            feature("a.opaque", Stage::Opaque),
            feature("b.opaque", Stage::Opaque),
            feature("a.prepare", Stage::Prepare),
        ]);
        assert_eq!(
            registry.names(),
            ["a.prepare", "a.opaque", "b.opaque", "a.post", "a.overlay"]
        );
    }

    #[test]
    fn before_and_after_constraints_are_respected_deterministically() {
        let registry = build([
            feature("post.bloom", Stage::PostProcess).after("post.exposure"),
            feature("post.exposure", Stage::PostProcess),
            feature("post.grain", Stage::PostProcess).before("post.bloom"),
            feature("post.vignette", Stage::PostProcess)
                .after("post.bloom")
                .after("missing.optional"),
        ]);
        assert_eq!(
            registry.names(),
            ["post.exposure", "post.grain", "post.bloom", "post.vignette"]
        );
        // Same set registered in another order: constraints still hold and
        // ties follow registration order.
        let other = build([
            feature("post.vignette", Stage::PostProcess).after("post.bloom"),
            feature("post.exposure", Stage::PostProcess),
            feature("post.bloom", Stage::PostProcess).after("post.exposure"),
        ]);
        assert_eq!(
            other.names(),
            ["post.exposure", "post.bloom", "post.vignette"]
        );
    }

    #[test]
    fn cycles_cross_stage_constraints_and_bad_names_are_rejected_without_changes() {
        let mut registry = build([feature("a.x", Stage::Opaque).after("a.y")]);
        let error = registry
            .register(Stub(feature("a.y", Stage::Opaque).after("a.x")))
            .unwrap_err();
        assert!(error.to_string().contains("cycle"), "{error}");
        assert_eq!(registry.names(), ["a.x"]);
        let error = registry
            .register(Stub(feature("a.z", Stage::Overlay).before("a.x")))
            .unwrap_err();
        assert!(error.to_string().contains("within one stage"), "{error}");
        for bad in ["", "Upper", "white space", &"x".repeat(65)] {
            assert!(
                registry
                    .register(Stub(feature(bad, Stage::Opaque)))
                    .is_err()
            );
        }
        assert!(
            registry
                .register(Stub(feature("engine.fake", Stage::Opaque)))
                .unwrap_err()
                .to_string()
                .contains("reserved")
        );
        assert!(
            registry
                .register(Stub(feature("a.x", Stage::Sky)))
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn per_view_overrides_resolve_with_required_and_single_winner_rules() {
        let mut registry = FeatureRegistry::default();
        registry
            .insert(
                feature("engine.output", Stage::Tonemap).required(),
                FeatureBody::Builtin,
            )
            .unwrap();
        registry
            .register(Stub(feature("post.aces", Stage::Tonemap)))
            .unwrap();
        registry
            .register(Stub(
                feature("debug.grid", Stage::Overlay).disabled_by_default(),
            ))
            .unwrap();
        let resolved = registry.resolve(&FeatureOverrides::default());
        assert!(!resolved.is_enabled("engine.output"), "superseded by aces");
        assert_eq!(
            resolved.get("engine.output").unwrap().activity,
            Activity::Superseded("post.aces".into())
        );
        assert!(resolved.is_enabled("post.aces"));
        assert!(!resolved.is_enabled("debug.grid"));
        let mut overrides = FeatureOverrides::default();
        overrides.set("post.aces", false);
        overrides.set("debug.grid", true);
        overrides.set("engine.output", false);
        overrides.set("not.registered", true);
        let resolved = registry.resolve(&overrides);
        assert!(resolved.is_enabled("engine.output"));
        assert_eq!(
            resolved.get("engine.output").unwrap().source,
            EnableSource::RequiredOverrideIgnored
        );
        assert!(!resolved.is_enabled("post.aces"));
        assert!(resolved.is_enabled("debug.grid"));
        assert_eq!(resolved.unknown, ["not.registered"]);
    }

    #[test]
    fn needs_are_the_union_of_running_features() {
        let registry = build([
            feature("a.ssao", Stage::PostProcess).needs(FeatureNeeds {
                normals: true,
                motion: false,
            }),
            feature("a.blur", Stage::PostProcess)
                .needs(FeatureNeeds {
                    normals: false,
                    motion: true,
                })
                .disabled_by_default(),
        ]);
        let resolved = registry.resolve(&FeatureOverrides::default());
        assert_eq!(
            registry.needs(&resolved),
            FeatureNeeds {
                normals: true,
                motion: false
            }
        );
    }

    #[test]
    fn absorb_merges_registries_or_fails_without_changes() {
        let mut base = build([feature("a.one", Stage::Opaque)]);
        let extra = build([feature("b.two", Stage::Opaque).before("a.one")]);
        base.absorb(extra).unwrap();
        assert_eq!(base.names(), ["b.two", "a.one"]);
        let duplicate = build([feature("a.one", Stage::Sky)]);
        assert!(base.absorb(duplicate).is_err());
        assert_eq!(base.len(), 2);
    }
}
