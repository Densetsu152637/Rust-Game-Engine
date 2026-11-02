//! HDR display output for window surfaces: the request (off by default),
//! the pure capability selection over what the wgpu 30 surface reports, and
//! the status the renderer exposes. See docs/rendering-hdr-output.md.
//!
//! wgpu 30 can select and report surface colour spaces
//! (`SurfaceConfiguration::color_space`, `SurfaceCapabilities::
//! format_capabilities`) and report the display's HDR state
//! (`Surface::display_hdr_info`). The engine uses two of them:
//! `Rgba16Float` + `ExtendedSrgbLinear` (scRGB) and `Rgb10a2Unorm` +
//! `Bt2100Pq` (HDR10). HDR is reported active only when the configured
//! surface is one of those; every other outcome is the SDR sRGB path with the
//! reason in [`HdrStatus::fallback_reason`].

use anyhow::{Context, Result, bail};
use engine_render_pipeline::hdr::{
    DEFAULT_PAPER_WHITE_NITS, DEFAULT_PEAK_NITS, HdrEncoding, HdrOutputSettings, validate_nits,
};
use wgpu::{
    DisplayHdrInfo, SurfaceColorSpace, SurfaceColorSpaces, SurfaceFormatCapabilities, TextureFormat,
};

/// Environment variable requesting HDR output: `off` (the default), `auto`
/// (scRGB, else PQ), `scrgb` or `pq`.
pub const HDR_ENV_VAR: &str = "ENGINE_HDR";
/// Paper white (nits of display-linear `1.0`, UI and diffuse white).
pub const HDR_PAPER_WHITE_ENV_VAR: &str = "ENGINE_HDR_PAPER_WHITE_NITS";
/// Upper bound of the displayed luminance, in nits (lowered to the display's
/// reported peak when the platform reports one).
pub const HDR_PEAK_ENV_VAR: &str = "ENGINE_HDR_PEAK_NITS";

/// Which HDR surface encodings a host will accept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HdrPreference {
    /// SDR sRGB output (the default).
    #[default]
    Off,
    /// scRGB when the surface offers it, else PQ.
    Auto,
    /// Only `Rgba16Float` + extended linear sRGB.
    ScRgb,
    /// Only `Rgb10a2Unorm` + BT.2100 PQ.
    Pq,
}

/// A request for HDR output. Defaults to off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrRequest {
    pub preference: HdrPreference,
    pub paper_white_nits: f32,
    pub peak_nits: f32,
}

impl Default for HdrRequest {
    fn default() -> Self {
        Self {
            preference: HdrPreference::Off,
            paper_white_nits: DEFAULT_PAPER_WHITE_NITS,
            peak_nits: DEFAULT_PEAK_NITS,
        }
    }
}

impl HdrRequest {
    /// A request for `preference` with the default paper white and peak.
    pub fn new(preference: HdrPreference) -> Self {
        Self {
            preference,
            ..Default::default()
        }
    }

    pub fn is_requested(&self) -> bool {
        self.preference != HdrPreference::Off
    }

    /// Paper white and peak must be valid (see
    /// [`engine_render_pipeline::hdr::validate_nits`]).
    pub fn validate(&self) -> Result<()> {
        validate_nits(self.paper_white_nits, self.peak_nits)
    }

    /// Parse the values of [`HDR_ENV_VAR`], [`HDR_PAPER_WHITE_ENV_VAR`] and
    /// [`HDR_PEAK_ENV_VAR`]; unset or blank values are the defaults. Unknown
    /// modes, non-numbers and invalid nits are an error naming the variable.
    pub fn from_values(
        mode: Option<&str>,
        paper_white: Option<&str>,
        peak: Option<&str>,
    ) -> Result<Self> {
        let preference = match mode.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            None | Some("" | "off" | "0" | "false" | "sdr") => HdrPreference::Off,
            Some("auto" | "on" | "1" | "true") => HdrPreference::Auto,
            Some("scrgb") => HdrPreference::ScRgb,
            Some("pq" | "hdr10") => HdrPreference::Pq,
            Some(other) => {
                bail!("{HDR_ENV_VAR}={other:?} is not an HDR mode; expected off, auto, scrgb or pq")
            }
        };
        let nits = |name: &str, value: Option<&str>, default: f32| -> Result<f32> {
            match value.map(str::trim).filter(|v| !v.is_empty()) {
                None => Ok(default),
                Some(text) => text
                    .parse::<f32>()
                    .with_context(|| format!("{name}={text:?} is not a number of nits")),
            }
        };
        let request = Self {
            preference,
            paper_white_nits: nits(
                HDR_PAPER_WHITE_ENV_VAR,
                paper_white,
                DEFAULT_PAPER_WHITE_NITS,
            )?,
            peak_nits: nits(HDR_PEAK_ENV_VAR, peak, DEFAULT_PEAK_NITS)?,
        };
        request
            .validate()
            .with_context(|| format!("{HDR_PAPER_WHITE_ENV_VAR} / {HDR_PEAK_ENV_VAR}"))?;
        Ok(request)
    }

    /// [`Self::from_values`] over the process environment.
    pub fn from_env() -> Result<Self> {
        let var = |name: &str| std::env::var(name).ok();
        Self::from_values(
            var(HDR_ENV_VAR).as_deref(),
            var(HDR_PAPER_WHITE_ENV_VAR).as_deref(),
            var(HDR_PEAK_ENV_VAR).as_deref(),
        )
    }
}

/// The HDR surface format, colour space and encoding a surface can be
/// configured with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HdrChoice {
    pub format: TextureFormat,
    pub color_space: SurfaceColorSpace,
    pub encoding: HdrEncoding,
}

/// scRGB: float surface, linear Rec.709, `1.0` = 80 nits.
pub const SCRGB_CHOICE: HdrChoice = HdrChoice {
    format: TextureFormat::Rgba16Float,
    color_space: SurfaceColorSpace::ExtendedSrgbLinear,
    encoding: HdrEncoding::ScRgb,
};
/// HDR10: 10-bit surface, Rec.2020 primaries, ST 2084 PQ.
pub const PQ_CHOICE: HdrChoice = HdrChoice {
    format: TextureFormat::Rgb10a2Unorm,
    color_space: SurfaceColorSpace::Bt2100Pq,
    encoding: HdrEncoding::Pq,
};

/// Outcome of [`select_hdr_surface`].
#[derive(Clone, Debug, PartialEq)]
pub enum HdrSelection {
    /// Configure the surface with this format and colour space.
    Hdr(HdrChoice),
    /// Stay on the SDR sRGB path. `reason` is `None` when HDR was not
    /// requested, otherwise why the request cannot be honoured.
    Sdr { reason: Option<String> },
}

fn offers(formats: &[SurfaceFormatCapabilities], choice: HdrChoice) -> bool {
    let space = choice
        .color_space
        .to_color_spaces()
        .unwrap_or(SurfaceColorSpaces::empty());
    formats
        .iter()
        .any(|f| f.format == choice.format && f.color_spaces.contains(space))
}

/// Pure capability selection. `formats` is
/// `SurfaceCapabilities::format_capabilities` (not `formats`, which never
/// lists HDR-only formats); `display` is `Surface::display_hdr_info`.
///
/// The surface capabilities say what the swapchain can be configured with,
/// not whether the monitor is in HDR mode (the DX12 backend advertises scRGB
/// and PQ unconditionally), so a display that positively reports it is not
/// HDR-capable ([`DisplayHdrInfo::coarse`] `high_dynamic_range ==
/// Some(false)`) keeps the SDR path. Unknown (`None`) is treated as capable.
pub fn select_hdr_surface(
    request: &HdrRequest,
    formats: &[SurfaceFormatCapabilities],
    display: &DisplayHdrInfo,
) -> HdrSelection {
    if !request.is_requested() {
        return HdrSelection::Sdr { reason: None };
    }
    if let Err(error) = request.validate() {
        return HdrSelection::Sdr {
            reason: Some(format!("invalid HDR request: {error}")),
        };
    }
    let candidates: &[HdrChoice] = match request.preference {
        HdrPreference::Off => &[],
        HdrPreference::Auto => &[SCRGB_CHOICE, PQ_CHOICE],
        HdrPreference::ScRgb => &[SCRGB_CHOICE],
        HdrPreference::Pq => &[PQ_CHOICE],
    };
    let Some(choice) = candidates.iter().copied().find(|c| offers(formats, *c)) else {
        let wanted = candidates
            .iter()
            .map(|c| format!("{:?} + {:?}", c.format, c.color_space))
            .collect::<Vec<_>>()
            .join(" or ");
        let offered = formats
            .iter()
            .map(|f| format!("{:?}{:?}", f.format, f.color_spaces))
            .collect::<Vec<_>>()
            .join(", ");
        return HdrSelection::Sdr {
            reason: Some(format!(
                "the surface offers no HDR configuration ({wanted}); it reports: [{offered}]"
            )),
        };
    };
    if display.coarse.and_then(|c| c.high_dynamic_range) == Some(false) {
        return HdrSelection::Sdr {
            reason: Some(
                "the display reports that it is not in an HDR mode (turn HDR on in the \
                 operating system display settings)"
                    .to_owned(),
            ),
        };
    }
    HdrSelection::Hdr(choice)
}

/// The peak the roll-off targets: the request's cap, lowered to the
/// platform-reported panel peak when there is one, and never below paper
/// white.
pub fn effective_peak_nits(request: &HdrRequest, display: &DisplayHdrInfo) -> f32 {
    let reported = display
        .luminance
        .and_then(|l| l.max_nits)
        .filter(|n| n.is_finite() && *n > 0.0);
    let peak = reported.map_or(request.peak_nits, |n| n.min(request.peak_nits));
    peak.max(request.paper_white_nits)
}

/// The output transform settings for a selected [`HdrChoice`].
pub fn output_settings(
    choice: HdrChoice,
    request: &HdrRequest,
    display: &DisplayHdrInfo,
) -> HdrOutputSettings {
    HdrOutputSettings {
        encoding: choice.encoding,
        paper_white_nits: request.paper_white_nits,
        peak_nits: effective_peak_nits(request, display),
    }
}

/// What the renderer actually does: requested vs active.
#[derive(Clone, Debug, PartialEq)]
pub struct HdrStatus {
    pub requested: bool,
    pub preference: HdrPreference,
    /// `true` only when the configured surface is an HDR format and colour
    /// space and the output transform is running.
    pub active: bool,
    /// The configured surface format (`None` for offscreen renderers).
    pub surface_format: Option<TextureFormat>,
    /// The configured surface colour space; `Srgb` on the SDR path, `None`
    /// for offscreen renderers.
    pub color_space: Option<SurfaceColorSpace>,
    pub encoding: Option<HdrEncoding>,
    /// Effective paper white and peak (nits) when active.
    pub paper_white_nits: Option<f32>,
    pub peak_nits: Option<f32>,
    /// The display's reported panel peak, when the platform reports one.
    pub display_peak_nits: Option<f32>,
    /// Why a request fell back to SDR.
    pub fallback_reason: Option<String>,
}

impl Default for HdrStatus {
    fn default() -> Self {
        Self::sdr(&HdrRequest::default(), None, None, None)
    }
}

impl HdrStatus {
    /// The SDR sRGB path; `reason` explains a fallback of a requested HDR
    /// output.
    pub fn sdr(
        request: &HdrRequest,
        surface_format: Option<TextureFormat>,
        color_space: Option<SurfaceColorSpace>,
        reason: Option<String>,
    ) -> Self {
        Self {
            requested: request.is_requested(),
            preference: request.preference,
            active: false,
            surface_format,
            color_space,
            encoding: None,
            paper_white_nits: None,
            peak_nits: None,
            display_peak_nits: None,
            fallback_reason: reason,
        }
    }

    /// The active HDR output of `settings` on `choice`.
    pub fn active(
        request: &HdrRequest,
        choice: HdrChoice,
        settings: &HdrOutputSettings,
        display: &DisplayHdrInfo,
    ) -> Self {
        Self {
            requested: true,
            preference: request.preference,
            active: true,
            surface_format: Some(choice.format),
            color_space: Some(choice.color_space),
            encoding: Some(choice.encoding),
            paper_white_nits: Some(settings.paper_white_nits),
            peak_nits: Some(settings.peak_nits),
            display_peak_nits: display.luminance.and_then(|l| l.max_nits),
            fallback_reason: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::{DisplayCoarseRange, DisplayLuminance};

    fn caps(entries: &[(TextureFormat, SurfaceColorSpaces)]) -> Vec<SurfaceFormatCapabilities> {
        entries
            .iter()
            .map(|&(format, color_spaces)| SurfaceFormatCapabilities {
                format,
                color_spaces,
            })
            .collect()
    }

    /// What the DX12 backend advertises (wgpu-hal dx12/adapter.rs).
    fn dx12_caps() -> Vec<SurfaceFormatCapabilities> {
        caps(&[
            (TextureFormat::Bgra8UnormSrgb, SurfaceColorSpaces::SRGB),
            (TextureFormat::Bgra8Unorm, SurfaceColorSpaces::SRGB),
            (
                TextureFormat::Rgb10a2Unorm,
                SurfaceColorSpaces::SRGB | SurfaceColorSpaces::BT2100_PQ,
            ),
            (
                TextureFormat::Rgba16Float,
                SurfaceColorSpaces::EXTENDED_SRGB_LINEAR,
            ),
        ])
    }

    fn sdr_only() -> Vec<SurfaceFormatCapabilities> {
        caps(&[
            (TextureFormat::Bgra8UnormSrgb, SurfaceColorSpaces::SRGB),
            (TextureFormat::Bgra8Unorm, SurfaceColorSpaces::SRGB),
        ])
    }

    fn hdr_display() -> DisplayHdrInfo {
        DisplayHdrInfo {
            luminance: Some(DisplayLuminance {
                max_nits: Some(800.0),
                sdr_white_nits: Some(240.0),
                ..Default::default()
            }),
            coarse: Some(DisplayCoarseRange {
                high_dynamic_range: Some(true),
                gamut: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn hdr_is_off_by_default_and_never_selected_unrequested() {
        assert!(!HdrRequest::default().is_requested());
        assert_eq!(
            select_hdr_surface(&HdrRequest::default(), &dx12_caps(), &hdr_display()),
            HdrSelection::Sdr { reason: None }
        );
    }

    #[test]
    fn auto_prefers_scrgb_then_pq() {
        let request = HdrRequest::new(HdrPreference::Auto);
        assert_eq!(
            select_hdr_surface(&request, &dx12_caps(), &hdr_display()),
            HdrSelection::Hdr(SCRGB_CHOICE)
        );
        let pq_only = caps(&[
            (TextureFormat::Bgra8UnormSrgb, SurfaceColorSpaces::SRGB),
            (TextureFormat::Rgb10a2Unorm, SurfaceColorSpaces::BT2100_PQ),
        ]);
        assert_eq!(
            select_hdr_surface(&request, &pq_only, &hdr_display()),
            HdrSelection::Hdr(PQ_CHOICE)
        );
    }

    #[test]
    fn an_explicit_encoding_is_not_substituted() {
        let pq = HdrRequest::new(HdrPreference::Pq);
        assert_eq!(
            select_hdr_surface(&pq, &dx12_caps(), &hdr_display()),
            HdrSelection::Hdr(PQ_CHOICE)
        );
        let scrgb_only = caps(&[(
            TextureFormat::Rgba16Float,
            SurfaceColorSpaces::EXTENDED_SRGB_LINEAR,
        )]);
        let HdrSelection::Sdr { reason } = select_hdr_surface(&pq, &scrgb_only, &hdr_display())
        else {
            panic!("PQ was requested and the surface has no PQ");
        };
        assert!(reason.unwrap().contains("Bt2100Pq"));
    }

    #[test]
    fn an_sdr_only_surface_falls_back_with_the_reported_formats() {
        for preference in [HdrPreference::Auto, HdrPreference::ScRgb, HdrPreference::Pq] {
            let HdrSelection::Sdr { reason } =
                select_hdr_surface(&HdrRequest::new(preference), &sdr_only(), &hdr_display())
            else {
                panic!("{preference:?} must fall back");
            };
            let reason = reason.expect("a requested HDR output explains its fallback");
            assert!(reason.contains("no HDR configuration"), "{reason}");
            assert!(reason.contains("Bgra8UnormSrgb"), "{reason}");
        }
    }

    #[test]
    fn a_float_format_without_the_scrgb_colour_space_is_not_hdr() {
        // Rgba16Float offered only as sRGB (or encoded extended sRGB) is not
        // scRGB: the colour space decides, not the texel format.
        for spaces in [
            SurfaceColorSpaces::SRGB,
            SurfaceColorSpaces::EXTENDED_SRGB,
            SurfaceColorSpaces::DISPLAY_P3,
        ] {
            let float_only = caps(&[(TextureFormat::Rgba16Float, spaces)]);
            let selection = select_hdr_surface(
                &HdrRequest::new(HdrPreference::ScRgb),
                &float_only,
                &hdr_display(),
            );
            assert!(matches!(selection, HdrSelection::Sdr { .. }), "{spaces:?}");
        }
    }

    #[test]
    fn a_display_that_reports_sdr_keeps_the_sdr_path_even_if_the_surface_offers_hdr() {
        let sdr_display = DisplayHdrInfo {
            coarse: Some(DisplayCoarseRange {
                high_dynamic_range: Some(false),
                gamut: None,
            }),
            ..Default::default()
        };
        let HdrSelection::Sdr { reason } = select_hdr_surface(
            &HdrRequest::new(HdrPreference::Auto),
            &dx12_caps(),
            &sdr_display,
        ) else {
            panic!("an SDR display must not report HDR active");
        };
        assert!(reason.unwrap().contains("not in an HDR mode"));
        // Unknown display state does not block (the info is advisory).
        assert!(matches!(
            select_hdr_surface(
                &HdrRequest::new(HdrPreference::Auto),
                &dx12_caps(),
                &DisplayHdrInfo::default()
            ),
            HdrSelection::Hdr(_)
        ));
    }

    #[test]
    fn an_invalid_request_falls_back_instead_of_configuring() {
        let request = HdrRequest {
            preference: HdrPreference::Auto,
            paper_white_nits: 500.0,
            peak_nits: 100.0,
        };
        let HdrSelection::Sdr { reason } =
            select_hdr_surface(&request, &dx12_caps(), &hdr_display())
        else {
            panic!("paper white above peak is invalid");
        };
        assert!(reason.unwrap().contains("invalid HDR request"));
    }

    #[test]
    fn the_peak_follows_the_reported_panel_and_never_drops_below_paper_white() {
        let request = HdrRequest::default();
        assert_eq!(effective_peak_nits(&request, &hdr_display()), 800.0);
        assert_eq!(
            effective_peak_nits(&request, &DisplayHdrInfo::default()),
            1000.0
        );
        let dim = DisplayHdrInfo {
            luminance: Some(DisplayLuminance {
                max_nits: Some(150.0),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(effective_peak_nits(&request, &dim), 203.0);
        let bad = DisplayHdrInfo {
            luminance: Some(DisplayLuminance {
                max_nits: Some(f32::NAN),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(effective_peak_nits(&request, &bad), 1000.0);
    }

    #[test]
    fn request_values_parse_and_validate() {
        let off = HdrRequest::from_values(None, None, None).unwrap();
        assert_eq!(off, HdrRequest::default());
        for (text, want) in [
            ("auto", HdrPreference::Auto),
            (" ScRGB ", HdrPreference::ScRgb),
            ("PQ", HdrPreference::Pq),
            ("hdr10", HdrPreference::Pq),
            ("off", HdrPreference::Off),
            ("", HdrPreference::Off),
        ] {
            assert_eq!(
                HdrRequest::from_values(Some(text), None, None)
                    .unwrap()
                    .preference,
                want,
                "{text:?}"
            );
        }
        let custom = HdrRequest::from_values(Some("auto"), Some("250"), Some("600")).unwrap();
        assert_eq!((custom.paper_white_nits, custom.peak_nits), (250.0, 600.0));
        let error = HdrRequest::from_values(Some("rainbow"), None, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains(HDR_ENV_VAR), "{error}");
        for (paper, peak) in [
            ("abc", None),
            ("nan", None),
            ("1000", Some("500")),
            ("203", Some("99999")),
            ("2", None),
        ] {
            assert!(
                HdrRequest::from_values(Some("auto"), Some(paper), peak).is_err(),
                "{paper} / {peak:?}"
            );
        }
    }

    #[test]
    fn status_reports_requested_versus_active() {
        let request = HdrRequest::new(HdrPreference::Auto);
        let off = HdrStatus::default();
        assert!(!off.requested && !off.active && off.fallback_reason.is_none());
        let fell_back = HdrStatus::sdr(
            &request,
            Some(TextureFormat::Bgra8UnormSrgb),
            Some(SurfaceColorSpace::Srgb),
            Some("no HDR".into()),
        );
        assert!(fell_back.requested && !fell_back.active);
        assert_eq!(fell_back.fallback_reason.as_deref(), Some("no HDR"));
        let display = hdr_display();
        let settings = output_settings(SCRGB_CHOICE, &request, &display);
        let active = HdrStatus::active(&request, SCRGB_CHOICE, &settings, &display);
        assert!(active.active && active.fallback_reason.is_none());
        assert_eq!(active.surface_format, Some(TextureFormat::Rgba16Float));
        assert_eq!(
            active.color_space,
            Some(SurfaceColorSpace::ExtendedSrgbLinear)
        );
        assert_eq!(active.peak_nits, Some(800.0));
        assert_eq!(active.display_peak_nits, Some(800.0));
    }
}
