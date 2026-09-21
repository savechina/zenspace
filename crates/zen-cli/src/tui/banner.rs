#![allow(dead_code)] // T079 banner types — state transitions are driven by app.rs + surface
//! Gateway-state banner (FR-023, T079).
//!
//! PURPOSE: Unified one-row banner rendering gateway lifecycle states in the
//!   inline viewport. Replaces the ad-hoc `status_hint` strings scattered
//!   across app.rs so there is ONE banner mechanism.
//!
//! USAGE: The `App` holds a `GatewayBannerState` field. Transitions are driven
//!   by `start_async_chat`, `poll_llm_response`, and `prewarm` events. The
//!   inline_ui render reads the banner state and renders a one-row slot.
//!
//! EXPECTED: Banner text matches contract-04 §Degraded-mode UX verbatim:
//!   `gateway: connecting…` → `gateway: ok (v1.0)` → on failure
//!   `gateway: offline — memory & agent features degraded (retrying)`.
//!
//! ERRORS: None — pure rendering data type. State transitions are infallible.

use std::fmt;

/// Gateway lifecycle state surfaced in the one-row banner slot (FR-023).
///
/// One row in the inline viewport layout budget. When `Hidden`, the banner
/// slot takes 0 rows; otherwise exactly 1 row. The inline_ui constraint
/// array accounts for this.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum GatewayBannerState {
    /// No gateway banner visible (default state).
    #[default]
    Hidden,
    /// Socket dial + handshake in flight.
    Connecting,
    /// Live and handshaked; carries the negotiated server version.
    Ok(String),
    /// Transport dead; gateway-backed features degraded until the next
    /// call retries the link.
    OfflineDegraded,
    /// Handshake refusal including version mismatch (-32001).
    /// Renders the server-provided `reason` and `recovery` VERBATIM.
    Refused { reason: String, recovery: String },
}

impl GatewayBannerState {
    /// Returns the banner text for this state. Empty string for `Hidden`.
    ///
    /// All text matches contract-04 §Degraded-mode UX verbatim. Handshake
    /// refusal renders the server-provided messages verbatim.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Hidden => String::new(),
            Self::Connecting => "gateway: connecting\u{2026}".to_string(),
            Self::Ok(version) => format!("gateway: ok (v{version})"),
            Self::OfflineDegraded => {
                "gateway: offline \u{2014} memory & agent features degraded (retrying)".to_string()
            }
            Self::Refused { reason, recovery } => {
                if recovery.is_empty() {
                    format!("gateway: refused \u{2014} {reason}")
                } else {
                    format!("gateway: refused \u{2014} {reason} ({recovery})")
                }
            }
        }
    }

    /// Whether the banner slot should render (non-Hidden).
    #[must_use]
    pub fn is_visible(&self) -> bool {
        *self != Self::Hidden
    }

    /// The height this banner takes in the viewport (0 or 1 rows).
    #[must_use]
    pub fn height(&self) -> u16 {
        if self.is_visible() { 1 } else { 0 }
    }
}

impl fmt::Display for GatewayBannerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.text())
    }
}

/// FR-023: sentinel encoding a handshake refusal (incl. version mismatch,
/// -32001) across the String-typed turn-done channel: the producer encodes
/// the server-provided `reason`/`recovery` VERBATIM (US-separated, so any
/// text is safe); `poll_llm_response` decodes into
/// [`GatewayBannerState::Refused`]. One mechanism — never status_hint.
pub(crate) const REFUSAL_MARKER: &str = "[[REFUSED]]";

pub(crate) fn refusal_marker(reason: &str, recovery: &str) -> String {
    format!("{REFUSAL_MARKER}{reason}\u{1f}{recovery}")
}

pub(crate) fn parse_refusal_marker(s: &str) -> Option<(String, String)> {
    let rest = s.strip_prefix(REFUSAL_MARKER)?;
    let (reason, recovery) = rest.split_once('\u{1f}').unwrap_or((rest, ""));
    Some((reason.to_string(), recovery.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_has_zero_height() {
        assert_eq!(GatewayBannerState::Hidden.height(), 0);
        assert!(!GatewayBannerState::Hidden.is_visible());
        assert!(GatewayBannerState::Hidden.text().is_empty());
    }

    #[test]
    fn connecting_text_matches_contract() {
        let state = GatewayBannerState::Connecting;
        assert_eq!(state.text(), "gateway: connecting\u{2026}");
        assert_eq!(state.height(), 1);
    }

    #[test]
    fn ok_includes_version() {
        let state = GatewayBannerState::Ok("1.0".to_string());
        assert_eq!(state.text(), "gateway: ok (v1.0)");
    }

    #[test]
    fn offline_degraded_text_matches_contract() {
        let state = GatewayBannerState::OfflineDegraded;
        assert_eq!(
            state.text(),
            "gateway: offline \u{2014} memory & agent features degraded (retrying)"
        );
    }

    #[test]
    fn refused_renders_verbatim() {
        let state = GatewayBannerState::Refused {
            reason: "protocol version mismatch".to_string(),
            recovery: "upgrade server to v1.1".to_string(),
        };
        assert_eq!(
            state.text(),
            "gateway: refused \u{2014} protocol version mismatch (upgrade server to v1.1)"
        );
    }

    #[test]
    fn default_is_hidden() {
        let state = GatewayBannerState::default();
        assert_eq!(state, GatewayBannerState::Hidden);
    }

    #[test]
    fn display_trait() {
        let state = GatewayBannerState::Connecting;
        assert_eq!(format!("{state}"), "gateway: connecting\u{2026}");
    }

    #[test]
    fn refusal_marker_roundtrips_verbatim() {
        let reason = "server v1.1 required (client v1.0)";
        let recovery = "run: zen serve stop && zen serve start";
        let m = super::refusal_marker(reason, recovery);
        let (r, rec) = super::parse_refusal_marker(&m).expect("parse");
        assert_eq!(r, reason);
        assert_eq!(rec, recovery);
    }

    #[test]
    fn refusal_marker_rejects_foreign_strings() {
        assert!(super::parse_refusal_marker("gateway: offline").is_none());
        assert!(super::parse_refusal_marker("[[CANCELLED]]").is_none());
    }
}
