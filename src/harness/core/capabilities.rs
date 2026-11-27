//! Model capability gating: which tools the active model may be offered.
//!
//! The harness already has ONE omission mechanism - the `disabled_tools` set
//! consulted by the header writer, the native tool definitions and the
//! dispatch gate (`is_tool_disabled`). Capability gating therefore does not
//! invent a parallel filter: it ADDS names to that set ([`Harness::refresh_tool_omissions`]),
//! so every existing surface (prompt, native tools, extractor, dispatch) stays
//! consistent for free.
//!
//! Two independent sources decide whether a model can read images, and both
//! are conservative by construction:
//!
//! 1. **The provider catalogs** (`Connector::is_vlm`, which resolves models.dev
//!    and OpenRouter entries). Only `Some(false)` - a positive catalog
//!    statement that the model is known and does not accept `image` - gates
//!    anything. `Some(true)` and `None` (unknown/stale/empty cache) never do.
//! 2. **The provider's own typed rejection** (`ConnectorError::ImageInputUnsupported`).
//!    That variant is only produced by aiface's strict classifier, which
//!    requires a 400/422 AND a body about images AND a marker stating the
//!    MODALITY is unsupported ("supports text input only", "cannot process
//!    images", …) - a malformed image, a broken tool argument or any other
//!    unrelated 400 never reaches it. A hit is recorded for the PROCESS
//!    lifetime, keyed by provider + model, because it is observed behavior of
//!    that deployment rather than a guess.
//!
//! Ambiguity always resolves to "capable": keeping a vision tool the model
//! cannot use costs one failed request, while wrongly hiding it strips a
//! capable model's visual abilities for the rest of the process.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use aiface::connector::Connector;

use super::Harness;

/// Cache namespace the provider catalogs live in (the same one
/// `discover_context_window` and `refresh_pricing_catalog` use).
#[cfg(not(test))]
const CATALOG_CACHE_DIR: Option<&str> = Some("morpheus/cache");

/// Tools that exist to feed images back to the model, and are therefore
/// omitted for a model proven text-only.
///
/// Only `computer_screenshot` qualifies: it is the single computer tool whose
/// result carries an inline PNG (`ScreenshotOutput::images`). `computer_apps`,
/// `computer_snapshot`, `computer_windows` and `computer_wait` return the
/// accessibility TREE (text), and `computer_act` / `computer_control` / `computer_pointer`
/// send input from text arguments - all of them stay useful without vision.
pub(crate) const VISION_DEPENDENT_TOOLS: &[&str] = &["computer_screenshot"];

/// Provider+model pairs whose inability to read images was observed on the
/// wire, for the lifetime of the process.
///
/// `Mutex<HashSet>` - not a `DashMap`/`RwLock`: writes are rare (at most one
/// per model) and reads happen once per loop entry / fallback switch.
fn wire_rejections() -> &'static Mutex<HashSet<String>> {
    static REJECTIONS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    REJECTIONS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Stable registry key for the active model. Includes the provider: two
/// gateways publishing the same bare id can differ in what they accept, and
/// an observed rejection under one says nothing about the other.
fn model_key(connector: &Connector) -> Option<String> {
    let model = connector.effective_model()?;
    let provider = connector.provider_name().unwrap_or("?");
    Some(format!("{provider}\u{0}{model}"))
}

/// Whether a provider already stated it cannot read images for this model.
fn wire_rejected(connector: &Connector) -> bool {
    let Some(key) = model_key(connector) else {
        return false;
    };
    wire_rejections()
        .lock()
        .map(|set| set.contains(&key))
        .unwrap_or(false)
}

/// Record the provider's own statement that this model cannot read images.
///
/// Returns `true` when the verdict is NEW (the caller can then skip redundant
/// work). Poisoned-lock is treated as "unknown" - a capability gate must
/// never turn a lock failure into a permanent restriction silently.
pub(crate) fn record_image_unsupported(connector: &Connector) -> bool {
    let Some(key) = model_key(connector) else {
        return false;
    };
    match wire_rejections().lock() {
        Ok(mut set) => set.insert(key),
        Err(_) => false,
    }
}

/// Whether the active model is known - not merely suspected - to be unable to
/// read images.
///
/// The wire verdict wins over the catalog: a provider that rejected an image
/// is authoritative about its own deployment, whatever the (possibly stale)
/// catalog claims. The catalog is only consulted when there is no wire
/// verdict, and only `Some(false)` counts.
pub(crate) fn vision_unsupported(connector: &Connector) -> bool {
    if wire_rejected(connector) {
        return true;
    }
    // Unit tests must not depend on the developer's catalog cache: the same
    // model would gate differently on two machines (see `build_lsp` for the
    // same determinism contract).
    #[cfg(test)]
    {
        false
    }
    #[cfg(not(test))]
    {
        connector.is_vlm(CATALOG_CACHE_DIR) == Some(false)
    }
}

impl Harness {
    /// Recompute the EFFECTIVE omission set: the user's own disabled tools
    /// plus the capability gates of the ACTIVE model.
    ///
    /// `disabled_tools` is what every surface reads (header, native tool
    /// definitions, extractor, dispatch), while `user_disabled_tools` keeps
    /// the user's choice intact. That separation lets a fallback switch or a
    /// freshly learned verdict recompute the gates without ever losing a tool
    /// the user disabled by hand (and vice versa).
    pub(super) fn refresh_tool_omissions(&mut self) {
        let text_only = vision_unsupported(&self.connector);
        let mut effective = self.user_disabled_tools.clone();
        if text_only {
            effective.extend(
                VISION_DEPENDENT_TOOLS
                    .iter()
                    .map(|name| (*name).to_string()),
            );
        }
        self.disabled_tools = effective;
        // The transport projection follows the same verdict: a resumed
        // session can still carry screenshot PNGs in its history even though
        // the capture tool is now omitted.
        self.context_manager.set_vision_enabled(!text_only);
    }

    /// Learn a capability verdict from the wire and apply it to the running
    /// loop.
    ///
    /// Called when a request failed with
    /// [`ConnectorError::ImageInputUnsupported`](aiface::connector::ConnectorError).
    /// The verdict is recorded for the PROCESS lifetime; this loop must also
    /// stop offering the gated tools immediately, so the native definitions
    /// and the inline header are re-rendered from the new omission set.
    ///
    /// Returns `true` when this was the first observation for the model.
    pub(super) fn learn_image_unsupported(&mut self) -> bool {
        let first = record_image_unsupported(&self.connector);
        self.refresh_tool_omissions();
        // Native mode: the tool array is the ONLY place schemas are sent, so
        // the retried request must be built from the new omission set.
        self.init_native_tools();
        // Inline mode: the schemas live in the system prompt. Re-rendering is
        // idempotent and additionally emits the "Tool Availability Changed"
        // block, so the model is told the tool is gone instead of calling it.
        self.format_header_context();
        first
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_lists_only_the_tools_that_emit_images() {
        // Guard against a future tool joining the computer set and being
        // silently over-gated: the list is deliberately explicit.
        assert_eq!(VISION_DEPENDENT_TOOLS, &["computer_screenshot"]);
    }

    #[test]
    fn a_wire_verdict_is_keyed_by_provider_and_model() {
        let connector = Connector::new("openrouter").expect("connector");
        let connector = connector.with_model("some-text-only-model");
        assert!(!vision_unsupported(&connector));
        assert!(record_image_unsupported(&connector));
        assert!(
            !record_image_unsupported(&connector),
            "a second observation is not new"
        );
        assert!(vision_unsupported(&connector));

        let other_model = Connector::new("openrouter")
            .expect("connector")
            .with_model("some-other-model");
        assert!(
            !vision_unsupported(&other_model),
            "a verdict must not leak across models"
        );
    }
}
