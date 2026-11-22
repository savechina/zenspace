//! Single source of truth for the `## Available tools` prompt preamble.
//!
//! Both prompt-assembly paths render this block in the cacheable static zone:
//! the streaming orchestrator (`zen_agent::ZenAgent`, Path A) and the
//! single-string executor (`executor::AgentExecutor`, Path B). Keeping one
//! renderer prevents the two contracts from drifting apart.

/// Render the `## Available tools` preamble for an agent-scoped tool manifest.
///
/// The preamble carries the fenced-JSON tool-call convention, knowledge-tier
/// guidance, and the language-matching rule, followed by the manifest body
/// (one `- name: description (args: ...)` line per granted tool). It is
/// injected verbatim at the head of the static zone so the prefix stays
/// byte-stable across turns for provider prompt caching.
///
/// # Parameters
///
/// - `manifest`: the agent-scoped tool list body, as produced by
///   `ZenAgent::tool_manifest`. Callers omit the preamble entirely when this
///   is empty.
///
/// # Returns
///
/// The complete block, starting with the `## Available tools` heading and
/// ending with the manifest bytes (no trailing separator).
#[must_use]
pub fn available_tools_preamble(manifest: &str) -> String {
    format!(
        "## Available tools\n\
         You can call tools to read/write files, fetch web pages, search the web, \
         and more. To call a tool, emit a fenced ```json block with the shape \
         {{\"tool\": \"<name>\", \"args\": {{ ... }}}} (or an array of such objects). \
         The runtime parses, dispatches, and feeds results back for the next round.\n\
         For knowledge-base and note questions, search the indexed tiers first \
         (tier2_search for keywords, tier3_search for meaning, tier4_search for \
         entity links) before falling back to raw fs.grep/fs.read scans; fs tool \
         paths anchor under the zen root (~/.zen) with bare names falling back to \
         the vault (~/.zen/vault).\n\
         Respond in the user's language — 除非用户要求，回答的语言与用户提问的语言保持一致。\n\n\
         {manifest}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preamble_carries_contract_markers_verbatim() {
        let rendered = available_tools_preamble("- fs.read: read files (args: {})");
        assert!(rendered.starts_with("## Available tools\n"));
        assert!(rendered.contains("除非用户要求"));
        assert!(rendered.contains("{\"tool\": \"<name>\", \"args\": { ... }}"));
        assert!(rendered.ends_with("- fs.read: read files (args: {})"));
    }

    #[test]
    fn preamble_keeps_manifest_bytes_intact() {
        let manifest = "- a: one (args: {})\n- b: two (args: {})";
        let rendered = available_tools_preamble(manifest);
        assert!(rendered.contains(manifest));
    }
}
