//! Prompt-cache segmentation (compile-hygiene ③, Q-A decision).
//!
//! Capability layer for provider-side prompt caching: a segmented prompt
//! ([`CacheSegments`]) plus the [`PromptCacheControl`] marker trait whose
//! default is a no-op, so providers without cache support behave
//! byte-identically to the pre-③ path. [`crate::providers::anthropic`]
//! implements the real behaviour over the raw `/v1/messages` endpoint
//! (rig-core's client does not expose `cache_control`).
//!
//! Scope logic (Constitution XV):
//! - Functionality: splits a compile-style prompt into stable
//!   system/doc/summary/whitelist blocks and marks cumulative cache
//!   breakpoints so repeat calls reuse the cached prefix.
//! - User impact: none on providers without support; on Anthropic the
//!   completion response carries cache usage fields for cost observability.
//! - Default: no-op everywhere until a caller (T046 compile LLM stage)
//!   opts in; `[agentic.cache]` config lands with that wiring — a key with
//!   no reader would be a phantom-key anti-pattern (T192 lesson).
//! - Interaction: callers gate on `supports_prompt_cache()` and fall back
//!   to [`CacheSegments::to_single_prompt`] + the plain complete path.

use serde::Serialize;

/// One block of a segmented prompt. Block order is the wire order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptSegment {
    pub kind: SegmentKind,
    pub text: String,
}

/// Role of a block within a compile-style prompt (OpenKB's
/// system/doc/summary/whitelist partition).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    /// Stable task instructions — identical across compile runs.
    System,
    /// Canonical page/document content under compilation.
    Doc,
    /// Rolling summary of already-compiled material.
    Summary,
    /// The compile whitelist (compile-hygiene ②): every name a wikilink may
    /// legally target.
    Whitelist,
}

/// A segmented prompt: system, doc blocks, summary, whitelist — in that
/// order, matching OpenKB's cache layout.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CacheSegments {
    pub segments: Vec<PromptSegment>,
}

impl CacheSegments {
    /// Assemble the canonical segment order from the compile inputs.
    pub fn build(system: &str, docs: &[String], summary: &str, whitelist: &[String]) -> Self {
        let mut segments = vec![PromptSegment {
            kind: SegmentKind::System,
            text: system.to_string(),
        }];
        segments.extend(docs.iter().map(|doc| PromptSegment {
            kind: SegmentKind::Doc,
            text: doc.clone(),
        }));
        segments.push(PromptSegment {
            kind: SegmentKind::Summary,
            text: summary.to_string(),
        });
        segments.push(PromptSegment {
            kind: SegmentKind::Whitelist,
            text: whitelist.join("\n"),
        });
        Self { segments }
    }

    /// The whole prompt as one plain string — the fallback a caller sends
    /// through the normal complete path when the provider has no cache
    /// support. Contains no cache markers by construction.
    pub fn to_single_prompt(&self) -> String {
        self.segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// Anthropic cache-usage echo (compile-hygiene ③ acceptance: the
/// `cache_creation_input_tokens` share must be observable).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CacheUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

/// Completion result from a cache-aware call: text plus the usage echo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedCompletion {
    pub text: String,
    pub usage: CacheUsage,
}

/// Marker trait for providers with prompt-cache support (Q-A: default
/// no-op — every provider without an impl, and every caller that does not
/// check, gets the plain path).
pub trait PromptCacheControl: Send + Sync {
    /// True when [`complete_cached`](Self::complete_cached) is usable.
    fn supports_prompt_cache(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_orders_segments_system_docs_summary_whitelist() {
        let segments = CacheSegments::build(
            "system prompt",
            &["doc one".into(), "doc two".into()],
            "summary so far",
            &["Alpha".to_string(), "Beta".to_string()],
        );
        let kinds: Vec<SegmentKind> = segments.segments.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                SegmentKind::System,
                SegmentKind::Doc,
                SegmentKind::Doc,
                SegmentKind::Summary,
                SegmentKind::Whitelist,
            ]
        );
        assert_eq!(segments.segments[4].text, "Alpha\nBeta");
    }

    #[test]
    fn to_single_prompt_has_no_cache_markers() {
        let segments =
            CacheSegments::build("sys", &[String::from("doc")], "sum", &[String::from("W")]);
        let flat = segments.to_single_prompt();
        assert_eq!(flat, "sys\n\ndoc\n\nsum\n\nW");
        assert!(!flat.contains("cache_control"));
    }

    #[test]
    fn default_trait_impl_is_noop() {
        struct Unsupported;
        impl PromptCacheControl for Unsupported {}
        let provider = Unsupported;
        assert!(!provider.supports_prompt_cache());
    }
}
