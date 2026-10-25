//! Runtime language switch: the `Localization` resource, per-locale text
//! fonts and the `locale.changed` event (`docs/localization.md`).
use super::*;
use crate::{
    events::EngineEvent,
    localization::{LOCALE_CHANGED_EVENT, Localization},
};

impl Runtime {
    /// The runtime localization (shared handle): load strings, switch locale,
    /// read statistics and diagnostics. Inactive when the project has no
    /// `locale_config` and nothing was loaded natively.
    pub fn localization(&self) -> Localization {
        self.app.world().resource::<Localization>().clone()
    }

    /// The current locale, for example `fr-CA`.
    pub fn locale(&self) -> String {
        self.localization().locale()
    }

    /// Switch language without reloading anything. The UI re-resolves every
    /// string on the next tick, the text engine's per-language fonts change at
    /// once and a `locale.changed` event is queued. `locale.unavailable` when no
    /// catalog covers the locale; the previous locale then stays.
    pub fn set_locale(&mut self, locale: &str) -> Result<()> {
        self.localization().set_locale(locale)?;
        self.sync_localization()
    }

    /// Turn pseudo-localization on or off at runtime (it keeps the other
    /// authored options).
    pub fn set_pseudo_localization(&mut self, enabled: bool) -> Result<()> {
        self.localization().set_pseudo_enabled(enabled)?;
        self.sync_localization()
    }

    /// Apply a changed localization generation to the text engine and queue
    /// `locale.changed` when the locale itself changed. Cheap when nothing
    /// changed: one lock and one integer comparison.
    pub(super) fn sync_localization(&mut self) -> Result<()> {
        let localization = self.localization();
        if !localization.active() {
            return Ok(());
        }
        let generation = localization.generation();
        if generation == self.localized_generation {
            return Ok(());
        }
        let (locale, fonts) = localization.with(|l| (l.locale().clone(), l.fonts()));
        if let Some(engine) = &self.content.text {
            let key = locale.as_str().to_ascii_lowercase();
            let mut fallback = self.content.base_fallback.clone();
            if !fonts.is_empty() {
                let mut list = fonts;
                for base in fallback.by_language.get(&key).into_iter().flatten() {
                    if !list.iter().any(|f| f.eq_ignore_ascii_case(base)) {
                        list.push(base.clone());
                    }
                }
                fallback = fallback.with_language(&key, list);
            }
            engine.set_fallback(fallback)?;
            engine.set_language(Some(locale.as_str()));
        }
        self.localized_generation = generation;
        let previous = std::mem::replace(&mut self.localized_locale, locale.to_string());
        if previous != locale.as_str() {
            crate::emit_event(
                self.app.world_mut(),
                EngineEvent {
                    name: LOCALE_CHANGED_EVENT.into(),
                    source: None,
                    target: None,
                    payload: serde_json::json!({
                        "locale": locale.as_str(),
                        "previous": previous,
                        "generation": generation,
                    }),
                },
            )?;
        }
        Ok(())
    }
}
