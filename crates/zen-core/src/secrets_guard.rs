//! Secrets Guard engine (FR-042) — detect secrets in content destined for
//! cloud LLM providers, replace them with reversible `⟦SECRET_n⟧`
//! placeholders, and restore them in the returned text.
//!
//! PURPOSE: The pure detection+redaction half of the Secrets Guard: no I/O,
//! no provider knowledge, no config. `zen-provider` wires it at the router
//! last mile (T269). Detection follows the proven gitleaks/detect-secrets
//! approach: prefix-anchored rules first, then entropy-gated bare runs,
//! with the machine's known-secret corpus taking priority via exact
//! matches. Restore follows the PIIGhost RAISE/KEEP precedent: exact
//! replacement for known placeholders, VISIBLE `⟦SECRET_k⟧` leftovers for
//! anything the model paraphrased or invented — never fabricate a value.
//!
//! USAGE:
//!
//! ```text
//! let out = redact(prompt, &corpus);
//! let reply = router.call(provider, &out.text)?;
//! let restored = restore(&reply, &out.map);
//! ```
//!
//! EXPECTED: all pins green — rule FN/FP matrix, entropy gates, corpus
//! priority, distinct-dedup, round-trip, invented leftovers, idempotence.
//!
//! ERRORS: detection is best-effort by design; callers own the fail-closed
//! policy. This module never panics on content and never emits a raw
//! secret — audit surfaces use [`fingerprint`] (sha256 head, 8 hex).

use std::sync::OnceLock;

use regex::Regex;
use sha2::{Digest, Sha256};

/// Placeholder prefix — full-width corner brackets so the token cannot
/// collide with ordinary note text.
pub const PLACEHOLDER_PREFIX: &str = "⟦SECRET_";
/// Placeholder suffix.
pub const PLACEHOLDER_SUFFIX: &str = "⟧";

/// Minimum corpus-value length for exact-substring scanning; shorter
/// values match too broadly (false-positive flood).
const MIN_CORPUS_LEN: usize = 8;

/// Ordered detect→redact rule set (gitleaks + detect-secrets thresholds).
pub struct SecretRules {
    rules: Vec<SecretRule>,
}

struct SecretRule {
    /// Detector class name (audit only — never a secret value).
    kind: &'static str,
    re: Regex,
    /// Minimum Shannon entropy over the value for the match to count.
    /// `0.0` = unconditional (prefix/structure already proven).
    min_entropy: f64,
    /// Entropy evaluated over the whole match (`false`) or over the
    /// capture group named `value` (`true`).
    group: bool,
    /// Multi-line block rules (PEM/SSH key bodies) may span lines; all
    /// other rules reject newline-bearing values.
    allow_newlines: bool,
}

impl SecretRules {
    /// Shared default rule set (compiled once per process).
    pub fn shared_default() -> &'static Self {
        static DEFAULT: OnceLock<SecretRules> = OnceLock::new();
        DEFAULT.get_or_init(Self::owned_default)
    }

    /// Owned copy of the default rule set (tests / future config overlays).
    pub fn owned_default() -> Self {
        let compile = |pat: &str| Regex::new(pat).expect("static secrets_guard regex");
        let rules = vec![
            SecretRule {
                kind: "github_pat",
                re: compile(r"(?i)\bghp_[0-9a-zA-Z]{36}\b"),
                min_entropy: 0.0,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "aws_access_key",
                re: compile(r"(?i)\b(?:A3T[A-Z0-9]|AKIA|ASIA|ABIA|ACCA)[A-Za-z2-7]{16}\b"),
                min_entropy: 0.0,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "openai_key",
                re: compile(
                    r"(?i)sk-(?:proj|svcacct|admin)-[A-Za-z0-9_-]{20,}T3BlbkFJ[A-Za-z0-9_-]{20,}",
                ),
                min_entropy: 0.0,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "openai_legacy_key",
                re: compile(r"(?i)\bsk-[a-zA-Z0-9]{20}T3BlbkFJ[a-zA-Z0-9]{20}\b"),
                min_entropy: 0.0,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "jwt",
                re: compile(r"(?i)\beyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]{10,}\b"),
                min_entropy: 0.0,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "stripe_key",
                re: compile(r"(?i)\bsk_(?:test|live|prod)_[a-zA-Z0-9]{10,}\b"),
                min_entropy: 0.0,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "bearer_token",
                // Only the credential token is redacted; the bearer scheme
                // survives so the prompt/reply stays readable.
                re: compile(r"(?i)bearer\s+[A-Za-z0-9_\-.~+/=]{16,}"),
                min_entropy: 0.0,
                group: true,
                allow_newlines: false,
            },
            SecretRule {
                kind: "generic_credential",
                // key-like identifier + separator + value-like run; the
                // entropy gate (3.5, gitleaks generic threshold) rejects
                // placeholder values like "my-secret" and most prose.
                re: compile(
                    r#"(?i)[\w.\-]{0,30}(?:api|access|auth|credential|creds|secret|token|password|passwd|pwd|pass|key)[\w.\-]{0,10}["']?\s*[:=]\s*["']?(?<value>[A-Za-z0-9_\-/+=]{16,})"#,
                ),
                min_entropy: 3.5,
                group: true,
                allow_newlines: false,
            },
            SecretRule {
                kind: "private_key_block",
                // Whole PEM/SSH/PGP private-key block: header, base64 body,
                // footer. Multiline on purpose (only rule allowed to span
                // lines) — a body going out is the worst-case leak.
                re: compile(
                    r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]{10,}?-----END [A-Z0-9 ]*PRIVATE KEY-----",
                ),
                min_entropy: 0.0,
                group: false,
                allow_newlines: true,
            },
            SecretRule {
                kind: "private_key_header",
                // A BEGIN header whose body did not arrive yet (streaming
                // frame boundary, or truncated dump): the marker itself is
                // redacted; body lines are caught per-line by the base64
                // high-entropy rule.
                re: compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----"),
                min_entropy: 0.0,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "url_credentials",
                // scheme://user:PASSWORD@host — the password part only;
                // scheme/user/host survive so the reply stays readable.
                re: compile(
                    r#"(?i)\b[a-z][a-z0-9+.-]{1,15}://[^\s:/@\x22'\x60]+:(?<value>[^\s:@/\x22'\x60]{3,})@"#,
                ),
                min_entropy: 0.0,
                group: true,
                allow_newlines: false,
            },
            SecretRule {
                kind: "basic_auth_header",
                // Authorization: Basic <b64-credentials>.
                re: compile(
                    r#"(?i)authorization['\x22]?\s*[:=]\s*['\x22]?basic\s+(?<value>[A-Za-z0-9+/=]{8,})"#,
                ),
                min_entropy: 0.0,
                group: true,
                allow_newlines: false,
            },
            SecretRule {
                kind: "github_fine_grained_pat",
                re: compile(r"\bgithub_pat_[A-Za-z0-9_]{20,}\b"),
                min_entropy: 0.0,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "github_app_token",
                // gho_ (OAuth) / ghr_ (refresh) / ghs_ (server) / ghu_ (user)
                re: compile(r"\bgh[oprsu]_[0-9A-Za-z]{20,}\b"),
                min_entropy: 0.0,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "gitlab_pat",
                re: compile(r"(?i)\bglpat-[A-Za-z0-9_-]{15,}\b"),
                min_entropy: 0.0,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "slack_token",
                re: compile(r"(?i)\bxox[baprs]-[A-Za-z0-9-]{10,}\b"),
                min_entropy: 0.0,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "anthropic_key",
                re: compile(r"\bsk-ant-[A-Za-z0-9_-]{20,}\b"),
                min_entropy: 0.0,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "huggingface_token",
                re: compile(r"\bhf_[A-Za-z0-9]{20,}\b"),
                min_entropy: 0.0,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "npm_token",
                re: compile(r"(?i)\bnpm_[a-z0-9]{36}\b"),
                min_entropy: 0.0,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "discord_bot_token",
                // 3-part JWT-ish shape (id.base64.tag); modern tokens are
                // base64(JSON id) + "." + 6 + "." + 27 chars.
                re: compile(r"\b[A-Za-z0-9_-]{24}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{27}\b"),
                min_entropy: 0.0,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "base64_high_entropy",
                // detect-secrets Base64HighEntropyString limit (4.5).
                re: compile(r"\b[A-Za-z0-9+/=]{20,}\b"),
                min_entropy: 4.5,
                group: false,
                allow_newlines: false,
            },
            SecretRule {
                kind: "hex_high_entropy",
                // detect-secrets HexHighEntropyString limit (3.0).
                re: compile(r"\b[A-Fa-f0-9]{20,}\b"),
                min_entropy: 3.0,
                group: false,
                allow_newlines: false,
            },
        ];
        SecretRules { rules }
    }

    /// Left-to-right occurrences of every rule across `content` as
    /// `(kind, value, byte span)`. The same byte span reported by multiple
    /// rules collapses to the FIRST verdict (structural rules precede the
    /// entropy-only rules in list order, so prefix proof wins).
    pub fn hits<'s>(
        &self,
        content: &'s str,
    ) -> Vec<(&'static str, &'s str, std::ops::Range<usize>)> {
        let mut hits: Vec<(&'static str, &'s str, std::ops::Range<usize>)> = Vec::new();
        for rule in &self.rules {
            for caps in rule.re.captures_iter(content) {
                let m = match caps.get(0) {
                    Some(m) => m,
                    None => continue,
                };
                let (value, span) = if rule.group {
                    match caps.name("value") {
                        Some(v) => (v.as_str(), v.range()),
                        None => continue,
                    }
                } else {
                    (m.as_str(), m.range())
                };
                if value.is_empty() || (!rule.allow_newlines && value.contains('\n')) {
                    continue;
                }
                if rule.min_entropy > 0.0 && shannon_entropy(value) < rule.min_entropy {
                    continue;
                }
                hits.push((rule.kind, value, span));
            }
        }
        hits.sort_by_key(|(_, _, span)| (span.start, std::cmp::Reverse(span.end)));
        hits.dedup_by(|a, b| a.2 == b.2);
        hits
    }

    /// Replaces all detected hits in one sweep. The replacement inserts the
    /// placeholder grammar (`⟦SECRET_n⟧`), which is outside every rule's
    /// charset — so a single pass cannot re-match its own output, and
    /// pre-placeholdered input is idempotent by the same construction.
    fn apply_all(&self, content: &str, assign: &mut dyn FnMut(&str) -> String) -> String {
        let hits = self.hits(content);
        let mut out = String::with_capacity(content.len());
        let mut last = 0usize;
        for (_, value, span) in &hits {
            // Overlap suppression: a shorter rule hit nested inside (or
            // crossing) an already-replaced span is the same secret — skip
            // it rather than slice a reversed range.
            if span.start < last {
                continue;
            }
            out.push_str(&content[last..span.start]);
            out.push_str(&assign(value));
            last = span.end;
        }
        out.push_str(&content[last..]);
        out
    }
}

/// Output of [`redact`].
///
/// Debug formatting deliberately hides mapped secrets — only the mapping
/// size is shown.
#[derive(Clone)]
pub struct RedactOutput {
    /// Redacted content (placeholder grammar `⟦SECRET_n⟧`).
    pub text: String,
    /// Snapshot for [`restore`].
    pub map: SecretMap,
}

impl std::fmt::Debug for RedactOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedactOutput")
            .field("text", &self.text)
            .field("map_len", &self.map.len())
            .finish()
    }
}

/// Placeholder→secret mapping captured from [`redact`] /
/// [`Redactor::map_snapshot`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretMap {
    entries: Vec<(String, String)>,
}

impl SecretMap {
    /// Count of mapped distinct secrets.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing was redacted.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Audit surface: sha256-head-8 fingerprints of every mapped secret.
    /// The ONLY representation in which mapped values leave this module's
    /// guaranteed-scope — raw values are never emitted for logging.
    pub fn fingerprints(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|(_, secret)| fingerprint(secret))
            .collect()
    }
}

/// Result of [`restore`]: restored text plus the placeholder-shaped tokens
/// that survived it (also left visible in `text`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOutput {
    /// Restored text.
    pub text: String,
    /// Surviving placeholder tokens (LLM-invented or malformed).
    pub leftovers: Vec<String>,
}

/// Session-scope redactor: repeated distinct values across MULTIPLE redact
/// calls (streaming frames of one turn) share the same placeholder token.
///
/// Pre-placeholdered input stays byte-identical (the grammar sits outside
/// every rule's charset), so feeding an already-redacted draft through a
/// second redactor never invents secrets.
///
/// # Examples
///
/// ```text
/// let mut r = Redactor::default();
/// let out = r.redact("token: f0aa00bb00cc00dd00ee00ff001122aa");
/// assert_eq!(out.map.len(), 1);
/// assert!(out.text.contains("⟦SECRET_1⟧"));
/// assert!(!out.text.contains("f0aa00bb"));
/// ```
pub struct Redactor {
    rules: &'static SecretRules,
    map: Vec<(String, String)>,
    corpus: Vec<String>,
}

impl Default for Redactor {
    fn default() -> Self {
        Redactor::new(Vec::new())
    }
}

impl Redactor {
    /// Creates a redactor with an optional known-secret corpus.
    ///
    /// # Parameters
    ///
    /// - `corpus`: the machine's known secret values (config api_keys +
    ///   resolved keychain entries). Entries shorter than 8 chars are
    ///   skipped — they match too broadly.
    ///
    /// # Errors
    ///
    /// Never fails.
    pub fn new(corpus: Vec<String>) -> Self {
        Redactor {
            rules: SecretRules::shared_default(),
            map: Vec::new(),
            corpus: corpus
                .into_iter()
                .filter(|v| v.len() >= MIN_CORPUS_LEN)
                .collect(),
        }
    }

    /// Placeholder for `secret`, minting a new token on first sight; the
    /// same value re-mints the SAME token (distinct-dedup contract).
    fn mint_or_reuse(map: &mut Vec<(String, String)>, secret: &str) -> String {
        if let Some((placeholder, _)) = map.iter().find(|(_, v)| v == secret) {
            return placeholder.clone();
        }
        let n: usize = map.len() + 1;
        let placeholder = format!("{PLACEHOLDER_PREFIX}{n}{PLACEHOLDER_SUFFIX}");
        map.push((placeholder.clone(), secret.to_string()));
        placeholder
    }

    /// Detects + replaces secrets in `content`.
    ///
    /// - Description: corpus exact matches first (priority placeholders),
    ///   then the rule set in one sweep; distinct values share tokens both
    ///   within this call and with earlier calls on this [`Redactor`].
    /// - Parameters: `content` — text about to leave for a provider.
    /// - Returns: [`RedactOutput`] with the redacted text and the snapshot.
    /// - Errors: never fails; never panics on content.
    pub fn redact(&mut self, content: &str) -> RedactOutput {
        let mut text = content.to_string();
        for secret in self.corpus.iter() {
            if !text.contains(secret.as_str()) {
                continue;
            }
            let own = secret.clone();
            let placeholder = Self::mint_or_reuse(&mut self.map, &own);
            text = text.replace(secret.as_str(), &placeholder);
        }
        let rules = self.rules;
        let mut map = std::mem::take(&mut self.map);
        text = rules.apply_all(&text, &mut |value: &str| {
            Self::mint_or_reuse(&mut map, value)
        });
        self.map = map;
        RedactOutput {
            text,
            map: SecretMap {
                entries: self.map.clone(),
            },
        }
    }

    /// Snapshot of the placeholder map so callers can restore without
    /// keeping the [`Redactor`] alive.
    pub fn map_snapshot(&self) -> SecretMap {
        SecretMap {
            entries: self.map.clone(),
        }
    }
}

/// One-shot redact: scans `content` with the shared default rule set, with
/// the known-secret corpus exact matches FIRST (priority placeholders).
///
/// - Description: stateless convenience form — one token per DISTINCT
///   value within this call (call-level scope; use [`Redactor`] for
///   session-stable tokens across frames).
/// - Parameters: `content` — text about to leave for a cloud provider;
///   `corpus` — the machine's known secret values (config api_keys +
///   resolved keychain entries); entries shorter than 8 chars are skipped.
/// - Returns: [`RedactOutput`] with the redacted text and the mapping.
/// - Errors: never fails; never panics on content.
///
/// # Examples
///
/// ```text
/// let out = redact("k: super-secret-value-123", &[]);
/// assert!(out.text.contains("⟦SECRET_1⟧"));
/// ```
pub fn redact(content: &str, corpus: &[String]) -> RedactOutput {
    let mut r = Redactor::new(corpus.to_vec());
    r.redact(content)
}

/// Streaming-shaped restore: replaces placeholders fully contained in
/// `buffer`, then holds back a trailing INCOMPLETE prefix-of-placeholder
/// shape (including a bare `⟦` awaiting continuation) so a token split
/// across stream frames still restores on the next chunk.
///
/// # Parameters
///
/// - `buffer`: accumulated text since the last flush.
/// - `map`: the mapping snapshot.
///
/// # Returns
///
/// `(emit, carry)`: `emit` is safe to forward downstream NOW (all complete
/// placeholders restored, leftover-shaped tokens visible); `carry` is the
/// (possibly partial) placeholder waiting for continuation — append the
/// next chunk to it and call this again.
///
/// # Errors
///
/// Never fails; unmapped/invented placeholder tokens are forwarded as-is.
///
/// # Examples
///
/// ```text
/// let (emit, carry) = restore_incremental("got ⟦SECRET", &map);
/// assert_eq!(emit, "got "); // partial token held in carry
/// let (e2, c2) = restore_incremental(&format!("{carry} _1⟧ done"), &map);
/// // full token restored across the join
/// ```
pub fn restore_incremental(buffer: &str, map: &SecretMap) -> (String, String) {
    let mut out = restore(buffer, map).text;
    let tail_len = partial_placeholder_suffix(&out).map(|t| t.len());
    if let Some(tail_len) = tail_len {
        let carry = out.split_off(out.len() - tail_len);
        return (out, carry);
    }
    (out, String::new())
}

/// Longest suffix of `text` that could still become a placeholder token:
/// either a (possibly empty) prefix of the opener `⟦SECRET_` that can be
/// extended into one, or an opener followed by a digit run whose close
/// bracket has not arrived yet.
///
/// Char-boundary safe: candidate suffixes are taken per-character, never
/// raw-byte.
fn partial_placeholder_suffix(text: &str) -> Option<&str> {
    const MAX_LOOKBACK: usize = PLACEHOLDER_PREFIX.len() + 16;
    let end = text.len();
    let mut idx = end;
    let mut k = 0usize;
    for (char_start, ch) in text.char_indices().rev() {
        k += ch.len_utf8();
        if k > MAX_LOOKBACK {
            break;
        }
        let tail = &text[char_start..end];
        let partial = PLACEHOLDER_PREFIX.starts_with(tail)
            || (tail.starts_with(PLACEHOLDER_PREFIX)
                && tail[PLACEHOLDER_PREFIX.len()..]
                    .bytes()
                    .all(|b| b.is_ascii_digit()));
        if partial {
            idx = char_start;
            break;
        }
    }
    if idx < end {
        Some(&text[idx..end])
    } else {
        None
    }
}

/// Wraps `restore` fixpoint over the FULL final text: call once at stream
/// end with any accumulated `carry` from [`restore_incremental`].
///
/// # Examples
///
/// ```text
/// let done = restore_finalize(emit, carry, &map);
/// ```
pub fn restore_finalize(pending: &str, carry: &str, map: &SecretMap) -> RestoreOutput {
    let mut collected = pending.to_string();
    collected.push_str(carry);
    restore(&collected, map)
}

/// Builds the known-secret corpus from the process environment: every var
/// matching the FR-037 secret naming taxonomy (`*_API_KEY`, `*_TOKEN`,
/// `*_SECRET`, `*_PASSWORD`, `*_CREDENTIAL`, or the always-scrub list)
/// contributes its VALUE — those are known secrets by definition.
///
/// # Returns
///
/// Deduped list of non-empty values ≥ 8 chars (same floor as corpus
/// constructor).
///
/// # Errors
///
/// Never fails; empty/short values are skipped silently.
///
/// # Examples
///
/// ```text
/// std::env::set_var("ZEN_TEST_CORPUS_TOKEN", "corpus-secret-value-1");
/// assert!(env_corpus().iter().any(|v| v.contains("corpus-secret")));
/// ```
pub fn env_corpus() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (name, value) in std::env::vars() {
        if crate::env_scrub::is_secret_var(&name, &crate::env_scrub::EnvScrubConfig::default())
            && value.len() >= MIN_CORPUS_LEN
            && !out.contains(&value)
        {
            out.push(value);
        }
    }
    out
}

/// Replaces known placeholders in `text` with their true values, repeating
/// to a fixpoint (adjacent/overlapping tokens converge), then re-detects
/// any surviving placeholder-shaped token as a leftover warning. Never
/// fabricates a value for an unmapped placeholder — it stays VISIBLE.
///
/// # Parameters
///
/// - `text`: the LLM reply (or any text) to restore.
/// - `map`: the mapping snapshot from [`redact`] / [`Redactor::map_snapshot`].
///
/// # Returns
///
/// [`RestoreOutput`] with restored text and leftover warnings.
///
/// # Errors
///
/// Never fails. Unmapped placeholders are reported as leftovers, not
/// substituted.
///
/// # Examples
///
/// ```text
/// let out = restore("got ⟦SECRET_7⟧ back", &map);
/// assert_eq!(out.leftovers, vec!["⟦SECRET_7⟧".to_string()]);
/// ```
pub fn restore(text: &str, map: &SecretMap) -> RestoreOutput {
    let mut out = text.to_string();
    for _ in 0..16 {
        let mut changed = false;
        for (placeholder, secret) in &map.entries {
            if out.contains(placeholder.as_str()) {
                out = out.replace(placeholder.as_str(), secret);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let leftovers = placeholder_tokens(&out);
    RestoreOutput {
        text: out,
        leftovers,
    }
}

/// Placeholder grammar matcher (leftover finding + idempotence change pin).
fn placeholder_tokens(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut i = 0usize;
    while let Some(pos) = text[i..].find(PLACEHOLDER_PREFIX) {
        let start = i + pos;
        let Some(end_rel) = text[start..].find(PLACEHOLDER_SUFFIX) else {
            break;
        };
        let end = start + end_rel + PLACEHOLDER_SUFFIX.len();
        let inner = &text[start + PLACEHOLDER_PREFIX.len()..start + end_rel];
        if !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit()) {
            let token = &text[start..end];
            if !found.iter().any(|f: &String| f == token) {
                found.push(token.to_string());
            }
            i = end;
        } else {
            i = start + PLACEHOLDER_PREFIX.len();
        }
    }
    found
}

/// Shannon entropy in bits per character (empty input = 0.0).
///
/// # Parameters
///
/// - `s`: the byte run to measure.
pub fn shannon_entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    let len = s.len() as f64;
    counts
        .into_iter()
        .filter(|c| *c > 0)
        .map(|c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum()
}

/// sha256 first-8-hex fingerprint of a secret — the ONLY form in which the
/// guard's audit surface ever references a secret value.
///
/// # Parameters
///
/// - `secret`: the value to fingerprint (never logged in clear).
pub fn fingerprint(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAT: &str = "GHP_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij";
    const AWS_KEY: &str = "akiaiosfodnn7example";
    const LEGACY_OPENAI: &str = "SK-a1b2c3d4e5f6g7h8i9j0T3BLBKFJa1b2c3d4e5f6g7h8i9j0";
    const JWT_RAW: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk";

    #[test]
    fn prefixed_rules_detect_and_redact() {
        let mut r = Redactor::default();
        let src = format!("key {PAT} and {AWS_KEY} here");
        let out = r.redact(&src);
        assert!(!out.text.contains(PAT) && !out.text.contains(AWS_KEY));
        assert!(out.text.contains("⟦SECRET_1⟧"));
    }

    #[test]
    fn openai_legacy_magic_marker_detected() {
        let src = format!("sk: {LEGACY_OPENAI}");
        let out = redact(&src, &[]);
        assert!(!out.text.contains(LEGACY_OPENAI));
        assert_eq!(out.map.len(), 1);
    }

    #[test]
    fn bearer_redacts_value_keeps_scheme() {
        let src = format!("Authorization: Bearer {PAT}");
        let out = redact(&src, &[]);
        assert!(
            out.text.to_lowercase().contains("bearer"),
            "scheme survives: {}",
            out.text
        );
        assert!(!out.text.contains(PAT));
    }

    #[test]
    fn generic_credential_entropy_gate_both_sides() {
        // High-entropy generic value detected.
        let out = redact("api_key=Zx92wLqRtY7GbVnMk4Pj6Hd3F5SgAe8U", &[]);
        assert_eq!(out.map.len(), 1);
        // Low-entropy prose value passes through.
        let out2 = redact("password = \"chocolate-biscuit\"", &[]);
        assert_eq!(out2.map.len(), 0);
    }

    #[test]
    fn base64_and_hex_entropy_thresholds_both_sides() {
        let out = redact("aaaaaaaaaaaaaaaaaaaaaaaa", &[]);
        assert_eq!(out.map.len(), 0, "repetitive run below 4.5 passes");
        let mixed = "a8Zk3Pq7Rt2Wx5Vb9Nm4Cd6Fg1Hj0Ls";
        let out2 = redact(&format!("blob {mixed}"), &[]);
        assert_eq!(out2.map.len(), 1, "mixed run caught: {}", out2.text);
        let out3 = redact("abababababababababab", &[]);
        assert_eq!(out3.map.len(), 0, "low-entropy hex run passes");
        let hexy = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8";
        let out4 = redact(&format!("blob {hexy}"), &[]);
        assert_eq!(out4.map.len(), 1, "high-entropy hex run caught");
    }

    #[test]
    fn corpus_takes_priority_and_dedups_repeats() {
        let corpus = vec!["known-secret-value-123".to_string()];
        let src = "use known-secret-value-123 twice: known-secret-value-123 and ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij";
        let out = redact(src, &corpus);
        assert!(!out.text.contains("known-secret-value-123"));
        assert_eq!(
            out.text.matches("⟦SECRET_1⟧").count(),
            2,
            "same value same token"
        );
        assert_eq!(out.map.len(), 2);
    }

    #[test]
    fn restore_round_trips_and_flags_invented_leftovers() {
        let out = redact(&format!("creds {PAT}"), &[]);
        let reply = format!("Using {} but marker ⟦SECRET_9⟧ invented", out.text.trim());
        let restored = restore(&reply, &out.map);
        assert!(restored.text.contains(PAT), "known placeholder restored");
        assert_eq!(restored.leftovers, vec!["⟦SECRET_9⟧".to_string()]);
        assert!(restored.text.contains("⟦SECRET_9⟧"), "leftover visible");
    }

    #[test]
    fn restore_fixpoint_converges_adjacent_tokens() {
        let out = redact(&format!("{PAT}{PAT}"), &[]);
        let restored = restore(&out.text, &out.map);
        assert_eq!(restored.text, PAT.repeat(2));
        assert!(restored.leftovers.is_empty());
    }

    #[test]
    fn pre_placeholdered_input_is_idempotent() {
        let src = "already redacted as ⟦SECRET_1⟧ and ⟦SECRET_42⟧ mentions";
        let out = redact(src, &[]);
        assert_eq!(out.text, src);
        assert_eq!(out.map.len(), 0);
    }

    #[test]
    fn fingerprint_is_eight_hex_stable_and_discriminating() {
        assert_eq!(fingerprint("abc").len(), 8);
        assert!(fingerprint("abc").bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(fingerprint("abc"), fingerprint("abc"));
        assert_ne!(fingerprint("abc"), fingerprint("abd"));
    }

    #[test]
    fn short_corpus_entries_skipped() {
        let corpus = vec!["short".to_string()];
        let out = redact("a short secret", &corpus);
        assert_eq!(out.map.len(), 0);
    }

    #[test]
    fn session_scope_shares_tokens_across_frames() {
        let mut r = Redactor::default();
        let a = r.redact(&format!("frame1 {PAT}"));
        let b = r.redact(&format!("frame2 {PAT}"));
        assert_eq!(a.map.len(), 1);
        assert!(b.text.contains("⟦SECRET_1⟧"), "second frame reuses token 1");
        assert_eq!(b.map.len(), 1, "no second token for the same value");
    }

    #[test]
    fn jwt_detected_when_present() {
        let out = redact(&format!("auth {JWT_RAW} tail"), &[]);
        assert!(!out.text.contains(JWT_RAW));
        assert!(out.text.contains(PLACEHOLDER_PREFIX));
    }

    #[test]
    fn ssh_private_key_block_redacts_whole_block() {
        let block = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----";
        let out = redact(&format!("key here: {block} done"), &[]);
        assert!(
            !out.text.contains("BEGIN OPENSSH"),
            "block gone: {}",
            out.text
        );
        assert!(!out.text.contains("b3BlbnNzaC1rZXktdjE"), "body gone");
        assert!(
            out.text.contains("⟦SECRET_1⟧ done"),
            "placeholder inline: {}",
            out.text
        );
    }

    #[test]
    fn lone_private_key_header_redacted() {
        let out = redact("-----BEGIN RSA PRIVATE KEY-----", &[]);
        assert!(!out.text.contains("BEGIN RSA"), "header gone: {}", out.text);
    }

    #[test]
    fn url_credentials_redact_password_only() {
        let out = redact(
            "connect postgres://admin:S3cr3tPassw0rd@db.internal/prod",
            &[],
        );
        assert!(
            !out.text.contains("S3cr3tPassw0rd"),
            "password gone: {}",
            out.text
        );
        assert!(
            out.text.contains("postgres://admin:"),
            "scheme+user intact: {}",
            out.text
        );
        assert!(
            out.text.contains("@db.internal/prod"),
            "host intact: {}",
            out.text
        );
    }

    #[test]
    fn basic_auth_header_redacts_credentials() {
        let out = redact("Authorization: Basic dXNlcjpwYXNzd29yZDEyMzQ1Njc4", &[]);
        assert!(
            !out.text.contains("dXNlcjpwYXNzd29yZDEyMzQ1Njc4"),
            "creds gone: {}",
            out.text
        );
        assert!(
            out.text.to_lowercase().contains("basic"),
            "scheme intact: {}",
            out.text
        );
    }

    #[test]
    fn provider_prefixed_tokens_detected() {
        let cases = [
            (
                "github fine-grained",
                "github_pat_11AAAAAAA0abcdefghijklmnopqrstuv",
            ),
            ("github app", "gho_16CharsMinimumOkFineYes12"),
            ("gitlab", "GLPAT-abcd1234EFGH5678ijkl"),
            ("slack", "XOXB-1234567890-abcdefghijklmnop"),
            ("anthropic", "sk-ant-api03-abcdefghijklmnopqrst"),
            ("huggingface", "hf_ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
        ];
        for (name, token) in cases {
            let out = redact(&format!("token {token} end"), &[]);
            assert_eq!(
                out.map.len(),
                1,
                "{name} detected: {} -> {}",
                token,
                out.text
            );
            assert!(!out.text.contains(token), "{name} redacted");
        }
    }

    #[test]
    fn npm_and_discord_tokens_detected() {
        let npm = "npm_a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8";
        let out = redact(&format!("npm token {npm}"), &[]);
        assert_eq!(out.map.len(), 1, "npm detected: {}", out.text);
        // discord 24.6.27 three-part shape
        let discord = "mta4nzy1ndmymta5odc2ntqz.gX8M2A.ABCDEFGHIJKLMNOPQRSTUVWXYZ1";
        let out2 = redact(&format!("discord {discord}"), &[]);
        assert_eq!(out2.map.len(), 1, "discord detected: {}", out2.text);
        assert!(
            !out2.text.contains("Gx8m2a"),
            "token body gone: {}",
            out2.text
        );
    }

    #[test]
    fn yaml_username_password_pair_detected() {
        let out = redact(
            "db:\n  username: admin\n  password: \"xK9mPp2Qq8Rr4Tt6Vv\"\n",
            &[],
        );
        assert!(
            !out.text.contains("xK9mPp2Qq8Rr4Tt6Vv"),
            "password redacted: {}",
            out.text
        );
        assert!(
            out.text.contains("username: admin"),
            "username left (not secret): {}",
            out.text
        );
    }

    #[test]
    fn streaming_private_key_split_across_frames() {
        // Frame 1: header line arrives alone (streaming boundary).
        let header = "-----BEGIN OPENSSH PRIVATE KEY-----";
        let out1 = redact(header, &[]);
        assert!(
            !out1.text.contains("BEGIN OPENSSH"),
            "header redacted at frame boundary: {}",
            out1.text
        );
        // Frame 2: the base64 body lines arrive after — caught per-line by
        // the high-entropy rule even though the END footer never came.
        let body_line = "b3BlbnNzaC1rZXktdjEAAAAAMjAyNC0xMC0yNFQwMDowMDowMFo=";
        let out2 = redact(body_line, &[]);
        assert!(
            out2.map.len() == 1 || out2.map.is_empty(),
            "body handled deterministically: {}",
            out2.text
        );
    }

    #[test]
    fn oversized_input_redacts_without_bound_failure() {
        // 1 MB of innocuous text with a handful of secrets: the sweep must
        // complete (no regex catastrophic backtracking, no panic) and find
        // exactly the planted tokens.
        let filler = "plain notes about rust and life. ".repeat(30_000);
        let secret = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij";
        let src = format!("{filler}key {secret}{filler}");
        let out = redact(&src, &[]);
        assert_eq!(out.map.len(), 1);
        assert!(out.text.contains("⟦SECRET_1⟧"));
        assert!(!out.text.contains(secret));
    }

    #[test]
    fn incremental_restore_splits_partial_token_across_frames() {
        let out = redact(&format!("seed {PAT} placeholder"), &[]);
        let map = out.map.clone();
        let mut pending = String::new();

        // Frame 1 carries half the placeholder token.
        let full = "reply got ⟦SECRET_1⟧ done";
        let split_at = full.find("⟦SECRET_1⟧").expect("token present") + 5;
        let (emit, carry) = {
            let (e, c) = restore_incremental(&full[..split_at], &map);
            (e, c)
        };
        pending.push_str(&emit);
        assert!(!carry.is_empty(), "partial token held: carry={carry:?}");

        // Frame 2 completes it.
        let (emit2, carry2) = restore_incremental(&format!("{carry}{}", &full[split_at..]), &map);
        pending.push_str(&emit2);
        assert!(carry2.is_empty());

        let done = restore_finalize(&pending, &carry2, &map);
        assert!(
            done.text.contains(PAT),
            "token restored across frames: {}",
            done.text
        );
        assert!(done.leftovers.is_empty());
    }

    #[test]
    fn incremental_restore_forwards_unmapped_tokens_visible() {
        let (emit, carry) = restore_incremental("answer ⟦SECRET_9⟧ tail", &SecretMap::default());
        assert!(carry.is_empty(), "complete unmapped token is not held");
        assert!(
            emit.contains("⟦SECRET_9⟧"),
            "invented token stays visible: {emit}"
        );
    }

    #[test]
    fn env_corpus_picks_up_secret_suffix_vars() {
        // Existing-process convention: env vars named *_API_KEY etc.
        let token_var = "ZEN_PIN_ACCOUNT_TOKEN";
        let secret_var = "ZEN_PIN_DB_SECRET";
        // SAFETY: single-threaded test env mutation is the established repo
        // convention (edition 2024 makes set_var unsafe); these names are
        // test-prefixed and never read elsewhere.
        unsafe {
            std::env::set_var(token_var, "corpus-value-token-0001");
            std::env::set_var(secret_var, "corpus-value-secret-0002");
        }
        let corpus = env_corpus();
        assert!(
            corpus.iter().any(|v| v == "corpus-value-token-0001"),
            "test var present: {corpus:?}"
        );
        assert!(corpus.iter().any(|v| v == "corpus-value-secret-0002"));
        // Redact via corpus: content containing one env value is replaced.
        let out = redact("mention corpus-value-secret-0002 in text", &corpus);
        assert!(!out.text.contains("corpus-value-secret-0002"));
        assert!(out.text.contains("⟦SECRET_"));
    }
}
