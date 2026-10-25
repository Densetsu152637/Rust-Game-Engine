//! Themes: named tokens, selector rules with partial style overrides,
//! keyframes, `extends` chains and the cascade that turns them into one
//! effective `UiStyle` per node.
use super::{
    animation::{MAX_RULE_TRANSITIONS, UiAnimation, UiKeyframes, UiTransition},
    selector::{self, MatchContext, Selector, style_error},
    tree::Hierarchy,
    *,
};
use std::{collections::BTreeMap, sync::Arc};

pub const MAX_THEME_RULES: usize = 512;
pub const MAX_THEME_TOKENS: usize = 256;
pub const MAX_THEME_KEYFRAMES: usize = 64;
/// Longest `extends` chain, the theme itself included.
pub const MAX_THEME_DEPTH: usize = 4;
/// Compiled rules (one per selector of every rule) in one composed theme.
pub const MAX_COMPILED_RULES: usize = 2048;

/// Names of themes, tokens and keyframes.
pub(super) fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

/// A style value or a `$token` reference (a string starting with `$`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum UiToken<T> {
    Token(String),
    Value(T),
}

/// Named values shared by the rules of a theme.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct UiTokens {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub colors: BTreeMap<String, [f32; 4]>,
    /// Spacing and sizes; used for numbers and as pixels for lengths.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub numbers: BTreeMap<String, f32>,
    /// Font keys.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub fonts: BTreeMap<String, String>,
}

fn lookup<T: Clone>(map: &BTreeMap<String, T>, reference: &str, what: &str) -> Result<T> {
    let name = reference
        .strip_prefix('$')
        .ok_or_else(|| style_error(format!("`{reference}` is not a $token reference")))?;
    map.get(name).cloned().ok_or_else(|| {
        Error::new(
            "ui.style.token_unknown",
            format!("Unknown {what} token ${name}"),
        )
    })
}

impl UiTokens {
    pub(super) fn color(&self, value: &UiToken<[f32; 4]>) -> Result<[f32; 4]> {
        match value {
            UiToken::Value(v) => Ok(*v),
            UiToken::Token(t) => lookup(&self.colors, t, "color"),
        }
    }
    pub(super) fn number(&self, value: &UiToken<f32>) -> Result<f32> {
        match value {
            UiToken::Value(v) => Ok(*v),
            UiToken::Token(t) => lookup(&self.numbers, t, "number"),
        }
    }
    pub(super) fn length(&self, value: &UiToken<UiLength>) -> Result<UiLength> {
        match value {
            UiToken::Value(v) => Ok(*v),
            UiToken::Token(t) => lookup(&self.numbers, t, "number").map(UiLength::Pixels),
        }
    }
    pub(super) fn font(&self, value: &str) -> Result<String> {
        if value.starts_with('$') {
            lookup(&self.fonts, value, "font")
        } else {
            Ok(value.to_owned())
        }
    }

    fn validate(&self) -> Result<()> {
        if self.colors.len() > MAX_THEME_TOKENS
            || self.numbers.len() > MAX_THEME_TOKENS
            || self.fonts.len() > MAX_THEME_TOKENS
        {
            return Err(style_error("A theme has at most 256 tokens per kind"));
        }
        let names = self
            .colors
            .keys()
            .chain(self.numbers.keys())
            .chain(self.fonts.keys());
        if !names.into_iter().all(|n| valid_name(n))
            || !self
                .colors
                .values()
                .all(|c| c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)))
            || !self
                .numbers
                .values()
                .all(|v| v.is_finite() && v.abs() <= MAX_UI_EXTENT)
            || !self.fonts.values().all(|f| f.len() <= 128)
        {
            return Err(style_error("Invalid theme token name or value"));
        }
        Ok(())
    }

    /// Tokens of `derived` layered over `base`.
    fn layered(base: &Self, derived: &Self) -> Self {
        let mut out = base.clone();
        out.colors.extend(derived.colors.clone());
        out.numbers.extend(derived.numbers.clone());
        out.fonts.extend(derived.fonts.clone());
        out
    }
}

macro_rules! style_delta {
    (
        direct: [$($df:ident: $dt:ty),* $(,)?],
        optional: [$($of:ident: $ot:ty),* $(,)?],
        colors: [$($cf:ident),* $(,)?],
        numbers: [$($nf:ident),* $(,)?],
        optional_numbers: [$($onf:ident),* $(,)?],
        lengths: [$($lf:ident),* $(,)?],
    ) => {
        /// Partial style: every present field overrides the matching `UiStyle`
        /// field, absent fields leave lower-priority values alone. Colors,
        /// numbers, lengths and `font` accept `$token` references.
        #[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
        #[serde(default, deny_unknown_fields, rename_all = "camelCase")]
        pub struct UiStyleDelta {
            $(#[serde(skip_serializing_if = "Option::is_none")] pub $df: Option<$dt>,)*
            $(#[serde(skip_serializing_if = "Option::is_none")] pub $of: Option<$ot>,)*
            $(#[serde(skip_serializing_if = "Option::is_none")] pub $cf: Option<UiToken<[f32; 4]>>,)*
            $(#[serde(skip_serializing_if = "Option::is_none")] pub $nf: Option<UiToken<f32>>,)*
            $(#[serde(skip_serializing_if = "Option::is_none")] pub $onf: Option<UiToken<f32>>,)*
            $(#[serde(skip_serializing_if = "Option::is_none")] pub $lf: Option<UiToken<UiLength>>,)*
            #[serde(skip_serializing_if = "Option::is_none")]
            pub font: Option<String>,
        }

        impl UiStyleDelta {
            /// Overlay the present fields onto `style`, resolving tokens.
            pub(super) fn apply(&self, style: &mut UiStyle, tokens: &UiTokens) -> Result<()> {
                $(if let Some(v) = &self.$df { style.$df = v.clone(); })*
                $(if let Some(v) = &self.$of { style.$of = Some(v.clone()); })*
                $(if let Some(v) = &self.$cf { style.$cf = tokens.color(v)?; })*
                $(if let Some(v) = &self.$nf { style.$nf = tokens.number(v)?; })*
                $(if let Some(v) = &self.$onf { style.$onf = Some(tokens.number(v)?); })*
                $(if let Some(v) = &self.$lf { style.$lf = tokens.length(v)?; })*
                if let Some(v) = &self.font { style.font = tokens.font(v)?; }
                Ok(())
            }
        }
    };
}

style_delta! {
    direct: [
        min_width: UiLength, min_height: UiLength, max_width: UiLength, max_height: UiLength,
        offset: [f32; 2], layout: UiLayout, position: UiPosition, inset: UiInset,
        margin: UiEdges, wrap: UiWrap, basis: UiLength, grid_columns: Vec<UiTrack>,
        grid_rows: Vec<UiTrack>, grid_auto_flow: UiGridFlow, grid_column: UiGridPlacement,
        grid_row: UiGridPlacement, clip: bool,
    ],
    optional: [
        aspect_ratio: f32, padding_edges: UiEdges, justify: UiJustify, align_items: UiAlign,
        align_self: UiAlign, align_content: UiJustify, justify_items: UiAlign,
        justify_self: UiAlign, grid_auto_columns: UiTrack, grid_auto_rows: UiTrack,
    ],
    colors: [background, foreground, accent],
    numbers: [padding, gap, font_scale, opacity, scale],
    optional_numbers: [row_gap, column_gap, grow, shrink],
    lengths: [width, height],
}

/// One selector rule. Rules apply in specificity order, ties in source order
/// (a derived theme's rules follow its base's).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct UiRule {
    /// Comma-separated selector list.
    pub selector: String,
    pub style: UiStyleDelta,
    /// Transitions for nodes this rule matches; the winning rule's list
    /// replaces lower-priority lists. Put them on the base rule, not on a
    /// `:hover` rule, so they also play when the state is left.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub transition: Vec<UiTransition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation: Option<UiAnimation>,
}

/// Authored theme (`engine.UiTheme`): tokens, rules and keyframes.
#[derive(Component, Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct UiTheme {
    pub version: u32,
    pub name: String,
    /// Base theme whose tokens, rules and keyframes this theme layers over.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
    pub tokens: UiTokens,
    pub rules: Vec<UiRule>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub keyframes: BTreeMap<String, UiKeyframes>,
}

impl Default for UiTheme {
    fn default() -> Self {
        Self {
            version: 1,
            name: "theme".into(),
            extends: None,
            tokens: UiTokens::default(),
            rules: vec![],
            keyframes: BTreeMap::new(),
        }
    }
}

impl UiTheme {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Add a rule with a selector and style (no motion).
    pub fn with_rule(mut self, selector: impl Into<String>, style: UiStyleDelta) -> Self {
        self.rules.push(UiRule {
            selector: selector.into(),
            style,
            ..Default::default()
        });
        self
    }

    pub fn with_color(mut self, name: impl Into<String>, color: [f32; 4]) -> Self {
        self.tokens.colors.insert(name.into(), color);
        self
    }

    pub fn with_number(mut self, name: impl Into<String>, value: f32) -> Self {
        self.tokens.numbers.insert(name.into(), value);
        self
    }

    /// Structural checks. Token references and keyframe names are resolved
    /// here only for themes without `extends`; derived themes are fully
    /// checked when composed.
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(style_error("Unsupported UiTheme version"));
        }
        if !valid_name(&self.name) || self.extends.as_deref().is_some_and(|e| !valid_name(e)) {
            return Err(style_error("Invalid theme name"));
        }
        if self.extends.as_deref() == Some(self.name.as_str()) {
            return Err(Error::new("ui.theme.cycle", "A theme cannot extend itself"));
        }
        if self.rules.len() > MAX_THEME_RULES || self.keyframes.len() > MAX_THEME_KEYFRAMES {
            return Err(style_error(
                "A theme has at most 512 rules and 64 keyframe sets",
            ));
        }
        self.tokens.validate()?;
        for (name, frames) in &self.keyframes {
            if !valid_name(name) {
                return Err(style_error("Invalid keyframes name"));
            }
            frames.validate()?;
        }
        for rule in &self.rules {
            selector::parse_list(&rule.selector)?;
            if rule.transition.len() > MAX_RULE_TRANSITIONS {
                return Err(style_error("A rule has at most 8 transitions"));
            }
            rule.transition
                .iter()
                .try_for_each(UiTransition::validate)?;
            if let Some(animation) = &rule.animation {
                animation.validate()?;
            }
        }
        if self.extends.is_none() {
            self.compile(&self.tokens, &self.keyframes, 0)?;
        }
        Ok(())
    }

    /// Compile this theme's rules, checking every token and keyframe name.
    fn compile(
        &self,
        tokens: &UiTokens,
        keyframes: &BTreeMap<String, UiKeyframes>,
        order_start: u32,
    ) -> Result<Vec<CompiledRule>> {
        let mut out = vec![];
        for rule in &self.rules {
            let mut scratch = UiStyle::default();
            rule.style.apply(&mut scratch, tokens)?;
            scratch.validate(&format!("rule `{}`", rule.selector))?;
            if let Some(animation) = &rule.animation
                && !keyframes.contains_key(&animation.name)
            {
                return Err(Error::new(
                    "ui.style.keyframes_unknown",
                    format!("Unknown keyframes `{}`", animation.name),
                ));
            }
            for selector in selector::parse_list(&rule.selector)? {
                out.push(CompiledRule {
                    specificity: selector.specificity(),
                    order: order_start + out.len() as u32,
                    selector,
                    style: rule.style.clone(),
                    transition: rule.transition.clone(),
                    animation: rule.animation.clone(),
                });
            }
        }
        Ok(out)
    }
}

#[derive(Clone, Debug)]
struct CompiledRule {
    selector: Selector,
    specificity: u32,
    order: u32,
    style: UiStyleDelta,
    transition: Vec<UiTransition>,
    animation: Option<UiAnimation>,
}

/// Named themes, resolved by name with `extends` chains.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiThemeSet {
    themes: BTreeMap<String, UiTheme>,
}

impl UiThemeSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace a theme after validating it.
    pub fn insert(&mut self, theme: UiTheme) -> Result<()> {
        theme.validate()?;
        self.themes.insert(theme.name.clone(), theme);
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Option<UiTheme> {
        self.themes.remove(name)
    }

    pub fn get(&self, name: &str) -> Option<&UiTheme> {
        self.themes.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.themes.keys().map(String::as_str)
    }

    /// The `extends` chain of `name`, base first.
    pub(super) fn chain(&self, name: &str) -> Result<Vec<&UiTheme>> {
        let mut chain = vec![];
        let mut cursor = Some(name);
        while let Some(current) = cursor {
            let theme = self.themes.get(current).ok_or_else(|| {
                Error::new("ui.theme.unknown", format!("Unknown UI theme `{current}`"))
            })?;
            if chain.len() >= MAX_THEME_DEPTH {
                return Err(Error::new(
                    "ui.theme.cycle",
                    "Theme extends chain too deep or cyclic (limit 4)",
                ));
            }
            if chain.iter().any(|t: &&UiTheme| t.name == theme.name) {
                return Err(Error::new("ui.theme.cycle", "Theme extends cycle"));
            }
            chain.push(theme);
            cursor = theme.extends.as_deref();
        }
        chain.reverse();
        Ok(chain)
    }

    /// Resolve `name` and its bases into one composed theme.
    pub fn compose(&self, name: &str) -> Result<UiComposedTheme> {
        UiComposedTheme::from_chain(&self.chain(name)?)
    }
}

/// A theme with its bases merged and selectors compiled, ready to cascade.
#[derive(Clone, Debug)]
pub struct UiComposedTheme {
    name: String,
    tokens: UiTokens,
    keyframes: BTreeMap<String, UiKeyframes>,
    /// Sorted by (specificity, source order): later wins.
    rules: Vec<CompiledRule>,
}

impl UiComposedTheme {
    /// Compose a standalone theme (no `extends`).
    pub fn new(theme: &UiTheme) -> Result<Self> {
        if theme.extends.is_some() {
            return Err(Error::new(
                "ui.theme.unknown",
                "Use UiThemeSet::compose for themes that extend another theme",
            ));
        }
        Self::from_chain(&[theme])
    }

    pub(super) fn from_chain(chain: &[&UiTheme]) -> Result<Self> {
        let mut tokens = UiTokens::default();
        let mut keyframes = BTreeMap::new();
        for theme in chain {
            tokens = UiTokens::layered(&tokens, &theme.tokens);
            keyframes.extend(theme.keyframes.clone());
        }
        let mut rules = vec![];
        for theme in chain {
            let start = rules.len() as u32;
            rules.extend(theme.compile(&tokens, &keyframes, start)?);
            if rules.len() > MAX_COMPILED_RULES {
                return Err(style_error("A composed theme has at most 2048 selectors"));
            }
        }
        rules.sort_by_key(|r| (r.specificity, r.order));
        Ok(Self {
            name: chain.last().map(|t| t.name.clone()).unwrap_or_default(),
            tokens,
            keyframes,
            rules,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn keyframes(&self, name: &str) -> Option<&UiKeyframes> {
        self.keyframes.get(name)
    }

    /// Effective style of every node of `root` (index-aligned with
    /// `root.tree.nodes`): defaults, then matching rules by specificity and
    /// order, then inline overrides, then inherited text properties.
    pub(super) fn cascade(
        &self,
        root: &UiRoot,
        hierarchy: &Hierarchy,
        states: &[UiStateFlags],
    ) -> Result<Vec<Computed>> {
        let nodes = &root.tree.nodes;
        let cx = MatchContext {
            nodes,
            parents: &hierarchy.parents,
            states,
        };
        let mut out: Vec<Option<Computed>> = vec![None; nodes.len()];
        // Parents before children, independent of storage order.
        let mut stack: Vec<usize> = hierarchy.roots.iter().rev().copied().collect();
        while let Some(index) = stack.pop() {
            let mut computed = Computed {
                style: UiStyle::default(),
                transition: vec![],
                animation: None,
            };
            for rule in &self.rules {
                if rule.selector.matches(&cx, index) {
                    rule.style.apply(&mut computed.style, &self.tokens)?;
                    if !rule.transition.is_empty() {
                        computed.transition = rule.transition.clone();
                    }
                    if let Some(animation) = &rule.animation {
                        computed.animation = Some(animation.clone());
                    }
                }
            }
            inline_over(&mut computed.style, &nodes[index].style);
            if let Some(parent) = hierarchy.parents[index].and_then(|p| out[p].as_ref()) {
                let default = UiStyle::DEFAULT;
                if computed.style.foreground == default.foreground {
                    computed.style.foreground = parent.style.foreground;
                }
                if computed.style.font.is_empty() {
                    computed.style.font = parent.style.font.clone();
                }
                if computed.style.font_scale == default.font_scale {
                    computed.style.font_scale = parent.style.font_scale;
                }
            }
            out[index] = Some(computed);
            stack.extend(hierarchy.children[index].iter().rev().copied());
        }
        Ok(out.into_iter().map(Option::unwrap_or_default).collect())
    }
}

/// Result of the cascade for one node.
#[derive(Clone, Debug, Default)]
pub(super) struct Computed {
    pub style: UiStyle,
    pub transition: Vec<UiTransition>,
    pub animation: Option<UiAnimation>,
}

/// Overlay every field of `inline` that differs from its default onto `base`.
/// Exhaustive destructuring makes a new `UiStyle` field a compile error here.
fn inline_over(base: &mut UiStyle, inline: &UiStyle) {
    let UiStyle {
        width,
        height,
        min_width,
        min_height,
        max_width,
        max_height,
        aspect_ratio,
        offset,
        layout,
        position,
        inset,
        margin,
        padding,
        padding_edges,
        gap,
        row_gap,
        column_gap,
        wrap,
        justify,
        align_items,
        align_self,
        align_content,
        justify_items,
        justify_self,
        grow,
        shrink,
        basis,
        grid_columns,
        grid_rows,
        grid_auto_columns,
        grid_auto_rows,
        grid_auto_flow,
        grid_column,
        grid_row,
        clip,
        background,
        foreground,
        accent,
        font,
        font_scale,
        opacity,
        scale,
    } = inline;
    let d = UiStyle::DEFAULT;
    macro_rules! over {
        ($($f:ident),* $(,)?) => {
            $(if *$f != d.$f { base.$f = $f.clone(); })*
        };
    }
    over!(
        width,
        height,
        min_width,
        min_height,
        max_width,
        max_height,
        aspect_ratio,
        offset,
        layout,
        position,
        inset,
        margin,
        padding,
        padding_edges,
        gap,
        row_gap,
        column_gap,
        wrap,
        justify,
        align_items,
        align_self,
        align_content,
        justify_items,
        justify_self,
        grow,
        shrink,
        basis,
        grid_columns,
        grid_rows,
        grid_auto_columns,
        grid_auto_rows,
        grid_auto_flow,
        grid_column,
        grid_row,
        clip,
        background,
        foreground,
        accent,
        font,
        font_scale,
        opacity,
        scale,
    );
}

/// Shared handle used by the runtime cache.
pub(super) type SharedTheme = Arc<UiComposedTheme>;
