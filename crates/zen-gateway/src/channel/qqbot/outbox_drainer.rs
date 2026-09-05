//! QQBot outbox drainer — the push face of the morning-brief heartbeat (FR-038, T101).
//!
//! The `morning-brief` scheduler worker (zen-agents, which must not depend on
//! zen-gateway) stages `logs/outbox/morning-brief-<date>.json`
//! (`{date, lines, chat_hint, sensitivity}`); this drainer, spawned from the
//! adapter's `run()` loop, delivers staged briefs with an ACTIVE send
//! (`msg_id=None`) on [`OUTBOX_DRAIN_INTERVAL`] ticks.
//!
//! Targeting: pushes go to exactly the chats with live `qq_bindings` rows —
//! chats the user already converses with. No new config surface, no probing
//! of never-seen openids. Bindings store no group/C2C kind, so each chat is
//! tried group-first with a C2C fallback; API errors never deliver, so a
//! wrong-kind attempt is a harmless warn, not a misdirected message.
//!
//! Sensitivity gate (mirror of `consumer_may_deliver` in zen-agents, kept
//! string-level so the gateway never imports agent internals): only `Public`
//! briefs deliver unless `consumer_allowlisted` is set. Skipped files stay in
//! place (never deleted) so an allowlisted drainer can pick them up later.
//! Production wiring passes `false` — personal push is a future opt-in knob.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tracing::{info, warn};

use super::api::QqBotApi;

/// Tick cadence for the outbox sweep. Briefs stage once daily at 9am; a
/// 5-minute tick bounds delivery latency after daemon restarts without
/// churning the QQ API.
pub const OUTBOX_DRAIN_INTERVAL: Duration = Duration::from_secs(300);

/// Staged morning-brief payload (producer: `MorningBriefWorker`).
#[derive(Debug, serde::Deserialize)]
struct OutboxBrief {
    #[allow(dead_code)]
    date: String,
    #[serde(default)]
    lines: Vec<String>,
    #[serde(default)]
    sensitivity: String,
}

/// Per-tick outcome, for logs and tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DrainReport {
    pub scanned: usize,
    pub delivered: usize,
    pub skipped_sensitivity: usize,
    pub skipped_no_targets: usize,
    pub failed: usize,
    pub corrupt: usize,
}

pub struct OutboxDrainer {
    api: Arc<QqBotApi>,
    outbox_dir: PathBuf,
    consumer_allowlisted: bool,
    seq: AtomicU32,
}

impl OutboxDrainer {
    pub fn new(api: Arc<QqBotApi>, outbox_dir: PathBuf, consumer_allowlisted: bool) -> Self {
        Self {
            api,
            outbox_dir,
            consumer_allowlisted,
            seq: AtomicU32::new(1),
        }
    }

    /// One sweep: every `morning-brief-*.json` (oldest first) is gated,
    /// pushed, and deleted on delivery to at least one chat. Files that
    /// are corrupt, sensitivity-skipped, targetless, or failed stay in
    /// place for a later tick or operator inspection — the drainer never
    /// deletes what it did not deliver.
    pub async fn drain_once(&self, chat_ids: &[String]) -> DrainReport {
        let mut report = DrainReport::default();
        let mut files = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.outbox_dir) else {
            return report;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("morning-brief-") && name.ends_with(".json") {
                files.push(entry.path());
            }
        }
        files.sort();
        for path in files {
            report.scanned += 1;
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "outbox read failed; leaving file");
                    report.corrupt += 1;
                    continue;
                }
            };
            let brief: OutboxBrief = match serde_json::from_str(&content) {
                Ok(b) => b,
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "outbox JSON corrupt; leaving file");
                    report.corrupt += 1;
                    continue;
                }
            };
            if brief.sensitivity != zen_core::types::Sensitivity::Public.to_string()
                && !self.consumer_allowlisted
            {
                warn!(path = %path.display(), sensitivity = %brief.sensitivity, "non-Public brief skipped; leaving file for allowlisted drainer");
                report.skipped_sensitivity += 1;
                continue;
            }
            let body = brief.lines.join("\n");
            if body.trim().is_empty() {
                warn!(path = %path.display(), "outbox brief has no lines; leaving file");
                report.corrupt += 1;
                continue;
            }
            if chat_ids.is_empty() {
                report.skipped_no_targets += 1;
                continue;
            }
            let mut delivered = false;
            for chat_id in chat_ids {
                if self.push_to_chat(chat_id, &body).await {
                    delivered = true;
                }
            }
            if delivered {
                match std::fs::remove_file(&path) {
                    Ok(()) => {
                        info!(path = %path.display(), "morning brief delivered; outbox file removed");
                        report.delivered += 1;
                    }
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "delivered but outbox file not removed; will resend next tick");
                        report.failed += 1;
                    }
                }
            } else {
                warn!(path = %path.display(), "all active sends failed; leaving file for next tick");
                report.failed += 1;
            }
        }
        report
    }

    /// Active send to one chat: group-first, C2C fallback (bindings carry
    /// no kind). A failed attempt delivers nothing — the fallback only
    /// runs when the first attempt errored.
    async fn push_to_chat(&self, chat_id: &str, body: &str) -> bool {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        if self
            .api
            .send_group_message(chat_id, body, None, seq)
            .await
            .is_ok()
        {
            return true;
        }
        self.api
            .send_c2c_message(chat_id, body, None, seq)
            .await
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::super::auth::QqBotAuth;
    use super::*;
    use axum::{Json, Router, extract::Path, routing::post};
    use std::sync::atomic::AtomicUsize;

    struct MockQq {
        base: String,
        sends: Arc<AtomicUsize>,
    }

    async fn mock_qq_server() -> MockQq {
        let sends = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&sends);
        let app = Router::new()
            .route(
                "/token",
                post(|| async {
                    Json(serde_json::json!({"access_token": "t", "expires_in": 7200}))
                }),
            )
            .route(
                "/v2/{kind}/{openid}/messages",
                post(move |Path((_kind, _openid)): Path<(String, String)>| {
                    let counter = Arc::clone(&counter);
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service()).await;
        });
        MockQq { base, sends }
    }

    fn stage(outbox: &std::path::Path, name: &str, payload: &serde_json::Value) {
        std::fs::write(outbox.join(name), serde_json::to_string(payload).unwrap()).unwrap();
    }

    fn brief_payload(sensitivity: &str) -> serde_json::Value {
        serde_json::json!({
            "date": "2026-09-04",
            "lines": ["Decisions: all clear", "People: no one new", "Watch-outs: all clear"],
            "chat_hint": "qqbot active send (msg_id=None)",
            "sensitivity": sensitivity,
        })
    }

    fn drainer_for(server: &MockQq, outbox: &std::path::Path) -> OutboxDrainer {
        let auth = Arc::new(QqBotAuth::new(
            "app".to_string(),
            "secret".to_string(),
            format!("{}/token", server.base),
        ));
        let api = Arc::new(QqBotApi::new(auth, server.base.clone()));
        OutboxDrainer::new(api, outbox.to_path_buf(), false)
    }

    #[tokio::test]
    async fn public_brief_is_sent_and_deleted() {
        let server = mock_qq_server().await;
        let dir = tempfile::TempDir::new().unwrap();
        stage(
            dir.path(),
            "morning-brief-2026-09-04.json",
            &brief_payload("Public"),
        );
        let drainer = drainer_for(&server, dir.path());
        let report = drainer.drain_once(&["chat1".to_string()]).await;
        assert_eq!(
            report,
            DrainReport {
                scanned: 1,
                delivered: 1,
                ..Default::default()
            },
            "got: {report:?}"
        );
        assert_eq!(server.sends.load(Ordering::SeqCst), 1);
        assert!(
            !dir.path().join("morning-brief-2026-09-04.json").exists(),
            "delivered file must be removed"
        );
    }

    #[tokio::test]
    async fn private_brief_is_skipped_and_kept() {
        let server = mock_qq_server().await;
        let dir = tempfile::TempDir::new().unwrap();
        stage(
            dir.path(),
            "morning-brief-2026-09-04.json",
            &brief_payload("Private"),
        );
        let drainer = drainer_for(&server, dir.path());
        let report = drainer.drain_once(&["chat1".to_string()]).await;
        assert_eq!(report.skipped_sensitivity, 1);
        assert_eq!(report.delivered, 0);
        assert_eq!(
            server.sends.load(Ordering::SeqCst),
            0,
            "no send for Private"
        );
        assert!(
            dir.path().join("morning-brief-2026-09-04.json").exists(),
            "skipped file must stay for an allowlisted drainer"
        );
    }

    #[tokio::test]
    async fn corrupt_file_is_left_in_place() {
        let server = mock_qq_server().await;
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("morning-brief-2026-09-04.json"),
            "{not json",
        )
        .unwrap();
        let drainer = drainer_for(&server, dir.path());
        let report = drainer.drain_once(&["chat1".to_string()]).await;
        assert_eq!(report.corrupt, 1);
        assert_eq!(server.sends.load(Ordering::SeqCst), 0);
        assert!(dir.path().join("morning-brief-2026-09-04.json").exists());
    }
}
