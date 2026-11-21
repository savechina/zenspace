//! Admission trust-ledger persistence (007 T003, migration 016).
//!
//! PURPOSE: durable backing for the host-enforced admission gate — decision
//! records (dedup-aware), immutable mandates (withdraw-only mutation), and
//! tickets carrying the reply address + hosted-turn binding.
//! USAGE: constructed by the qqbot intake seam and the review surfaces against
//! the daemon's state.db [`SqliteClient`].
//! ERRORS: SQLite failures surface as [`SqliteError`]; a replayed trigger is
//! NOT an error — it is recorded as a `duplicate` row and reported through
//! [`DecisionOutcome::Duplicate`] (FR-011, research D8).

use uuid::Uuid;

use crate::client::{Result, SqliteClient, SqliteError};

/// One decision-record insert, caller-minted id (mirrors `TaskCheckpoint`).
#[derive(Debug, Clone)]
pub struct DecisionRecord<'a> {
    pub id: &'a str,
    pub source: &'a str,
    pub event_id: Option<&'a str>,
    pub content_hash: &'a str,
    pub sender: &'a str,
    pub thread_key: &'a str,
    pub content: &'a str,
    /// would_accept|rejected|admitted|unresolved|duplicate (migration 016).
    pub disposition: &'a str,
    pub matched_rule: Option<&'a str>,
    pub scope: Option<&'a str>,
    pub accept_mode: &'a str,
    pub strength: Option<&'a str>,
    pub policy_version: &'a str,
    pub reason: Option<&'a str>,
    pub created_at: i64,
}

/// Outcome of [`AdmissionRepo::record_decision`]: a fresh decision row, or a
/// replayed trigger recorded as a `duplicate` row (never re-admitted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionOutcome {
    New,
    Duplicate { id: String },
}

/// Filter for [`AdmissionRepo::list_decisions`] — either bound is optional.
#[derive(Debug, Clone, Default)]
pub struct DispositionFilter {
    pub disposition: Option<String>,
    pub since: Option<i64>,
}

/// One mandate grant; the repo mints `mandate_id` (returned to the caller).
#[derive(Debug, Clone)]
pub struct MandateGrant<'a> {
    pub decision_id: &'a str,
    pub matched_rule: Option<&'a str>,
    pub scope: &'a str,
    pub strength: &'a str,
    pub accept_mode: &'a str,
    pub policy_version: &'a str,
    pub granted_at: i64,
}

/// One ticket open; the repo mints `ticket_id` (returned to the caller).
#[derive(Debug, Clone)]
pub struct TicketOpen<'a> {
    pub mandate_id: &'a str,
    pub reply_address: &'a str,
    pub turn_ref: Option<&'a str>,
    pub created_at: i64,
}

/// One `admission_decisions` row.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DecisionRow {
    pub id: String,
    pub source: String,
    pub event_id: Option<String>,
    pub content_hash: String,
    pub sender: String,
    pub thread_key: String,
    pub content: String,
    pub disposition: String,
    pub matched_rule: Option<String>,
    pub scope: Option<String>,
    pub accept_mode: String,
    pub strength: Option<String>,
    pub policy_version: String,
    pub reason: Option<String>,
    pub created_at: i64,
}

/// One `admission_mandates` row.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct MandateRow {
    pub mandate_id: String,
    pub decision_id: String,
    pub matched_rule: Option<String>,
    pub scope: String,
    pub strength: String,
    pub accept_mode: String,
    pub policy_version: String,
    pub granted_at: i64,
    pub withdrawn_at: Option<i64>,
    pub withdrawn_reason: Option<String>,
}

/// One `admission_tickets` row.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct TicketRow {
    pub ticket_id: String,
    pub mandate_id: String,
    /// open|ended
    pub status: String,
    /// ready|failed|dropped|stopped (NULL while open)
    pub report_state: Option<String>,
    pub reply_address: String,
    pub turn_ref: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

const INSERT_DECISION_SQL: &str = "INSERT INTO admission_decisions \
     (id, source, event_id, content_hash, sender, thread_key, content, disposition, \
      matched_rule, scope, accept_mode, strength, policy_version, reason, created_at) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)";

/// Dedup collisions are the two unique indexes on `admission_decisions`
/// (`source,event_id` and the partial `source,content_hash`); a PK collision
/// on `id` is a caller bug and must surface, not masquerade as a replay.
fn is_duplicate_delivery(err: &rusqlite::Error) -> bool {
    match err {
        rusqlite::Error::SqliteFailure(e, Some(msg)) => {
            e.code == rusqlite::ErrorCode::ConstraintViolation
                && msg.contains("admission_decisions.source")
                && (msg.contains("event_id") || msg.contains("content_hash"))
        }
        _ => false,
    }
}

/// Admission ledger repository (Principle XII: holds `&SqliteClient`, async
/// methods only).
pub struct AdmissionRepo<'a> {
    client: &'a SqliteClient,
}

impl<'a> AdmissionRepo<'a> {
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
    }

    /// Read the runtime accept-mode override row (migration 017).
    ///
    /// `Ok(None)` when no row was written (fresh database, or after
    /// [`Self::clear_mode_override`]) — the caller falls back to the
    /// configured `[channels.qqbot] accept_mode`. An invalid stored value is
    /// the caller's E11 concern (`AcceptMode::parse` warns + Observe).
    ///
    /// # Errors
    /// SQLite failure — callers on the hot path fail open to config.
    pub async fn mode_override(&self) -> Result<Option<String>> {
        let row = sqlx::query_scalar::<_, String>(
            "SELECT accept_mode FROM admission_mode_override WHERE id = 1",
        )
        .fetch_optional(self.client.pool())
        .await?;
        Ok(row)
    }

    /// Write the runtime accept-mode override (single-row UPSERT on `id = 1`).
    ///
    /// The CLI write path validates the value first (`off|observe|auto`);
    /// the daemon picks the row up on the next inbound message — no restart.
    /// Config remains untouched: the override WINS until cleared.
    ///
    /// # Errors
    /// SQLite failure.
    pub async fn set_mode_override(&self, mode: &str) -> Result<()> {
        // `writer().call` needs a 'static closure — own the param.
        let mode = mode.to_string();
        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO admission_mode_override(id, accept_mode) \
                     VALUES(1, ?1) \
                     ON CONFLICT(id) DO UPDATE SET \
                         accept_mode = excluded.accept_mode, \
                         updated_at = datetime('now')",
                    rusqlite::params![mode],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)
    }

    /// Clears the runtime override row: the effective mode falls back to
    /// config. Returns whether a row existed.
    ///
    /// # Errors
    /// SQLite failure.
    pub async fn clear_mode_override(&self) -> Result<bool> {
        self.client
            .writer()
            .call(|conn| Ok(conn.execute("DELETE FROM admission_mode_override WHERE id = 1", [])?))
            .await
            .map(|deleted| deleted > 0)
            .map_err(SqliteError::TokioRusqlite)
    }

    /// Insert one decision row, dedup-aware (FR-011): a replay of the same
    /// `(source, event_id)` — or, for sources without stable ids, the same
    /// `(source, content_hash)` — is recorded as a `duplicate` row with a
    /// fresh id and never re-admitted. The duplicate row carries a synthetic
    /// non-NULL `event_id` so it stays outside both unique indexes.
    ///
    /// # Errors
    /// SQLite failure other than a dedup collision (e.g. a duplicate primary
    /// key, which is a caller bug).
    pub async fn record_decision(&self, rec: DecisionRecord<'_>) -> Result<DecisionOutcome> {
        let DecisionRecord {
            id,
            source,
            event_id,
            content_hash,
            sender,
            thread_key,
            content,
            disposition,
            matched_rule,
            scope,
            accept_mode,
            strength,
            policy_version,
            reason,
            created_at,
        } = rec;
        let id = id.to_string();
        let source = source.to_string();
        let event_id = event_id.map(str::to_string);
        let content_hash = content_hash.to_string();
        let sender = sender.to_string();
        let thread_key = thread_key.to_string();
        let content = content.to_string();
        let disposition = disposition.to_string();
        let matched_rule = matched_rule.map(str::to_string);
        let scope = scope.map(str::to_string);
        let accept_mode = accept_mode.to_string();
        let strength = strength.map(str::to_string);
        let policy_version = policy_version.to_string();
        let reason = reason.map(str::to_string);

        self.client
            .writer()
            .call(move |conn| {
                let inserted = conn.execute(
                    INSERT_DECISION_SQL,
                    rusqlite::params![
                        id,
                        source,
                        event_id,
                        content_hash,
                        sender,
                        thread_key,
                        content,
                        disposition,
                        matched_rule,
                        scope,
                        accept_mode,
                        strength,
                        policy_version,
                        reason,
                        created_at
                    ],
                );
                match inserted {
                    Ok(_) => Ok(DecisionOutcome::New),
                    Err(err) if is_duplicate_delivery(&err) => {
                        let duplicate_id = Uuid::now_v7().to_string();
                        let duplicate_event_id = format!("dup:{duplicate_id}");
                        conn.execute(
                            INSERT_DECISION_SQL,
                            rusqlite::params![
                                duplicate_id,
                                source,
                                duplicate_event_id,
                                content_hash,
                                sender,
                                thread_key,
                                content,
                                "duplicate",
                                matched_rule,
                                scope,
                                accept_mode,
                                strength,
                                policy_version,
                                "duplicate",
                                created_at
                            ],
                        )?;
                        Ok(DecisionOutcome::Duplicate { id: duplicate_id })
                    }
                    Err(err) => Err(err.into()),
                }
            })
            .await
            .map_err(SqliteError::TokioRusqlite)
    }

    /// Grant an immutable mandate (FR-013). Returns the minted `mandate_id`.
    ///
    /// # Errors
    /// SQLite failure, including a foreign-key miss on `decision_id`.
    pub async fn grant_mandate(&self, grant: MandateGrant<'_>) -> Result<String> {
        let MandateGrant {
            decision_id,
            matched_rule,
            scope,
            strength,
            accept_mode,
            policy_version,
            granted_at,
        } = grant;
        let mandate_id = Uuid::now_v7().to_string();
        let id = mandate_id.clone();
        let decision_id = decision_id.to_string();
        let matched_rule = matched_rule.map(str::to_string);
        let scope = scope.to_string();
        let strength = strength.to_string();
        let accept_mode = accept_mode.to_string();
        let policy_version = policy_version.to_string();
        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO admission_mandates \
                     (mandate_id, decision_id, matched_rule, scope, strength, accept_mode, \
                      policy_version, granted_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        id,
                        decision_id,
                        matched_rule,
                        scope,
                        strength,
                        accept_mode,
                        policy_version,
                        granted_at
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(mandate_id)
    }

    /// Withdraw a mandate (FR-013/FR-014): UPDATE-only on
    /// `withdrawn_at`/`withdrawn_reason`; scope and every other granted
    /// column are never touched. Returns `false` when already withdrawn
    /// (idempotent) or unknown.
    ///
    /// # Errors
    /// SQLite write failure.
    pub async fn withdraw_mandate(&self, mandate_id: &str, reason: &str) -> Result<bool> {
        let mandate_id = mandate_id.to_string();
        let reason = reason.to_string();
        let changed = self
            .client
            .writer()
            .call(move |conn| {
                Ok(conn.execute(
                    "UPDATE admission_mandates \
                     SET withdrawn_at = CAST(strftime('%s','now') AS INTEGER), \
                         withdrawn_reason = ?2 \
                     WHERE mandate_id = ?1 AND withdrawn_at IS NULL",
                    rusqlite::params![mandate_id, reason],
                )?)
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(changed == 1)
    }

    /// Open a ticket for an admitted mandate. Returns the minted `ticket_id`.
    ///
    /// # Errors
    /// SQLite failure, including a foreign-key miss on `mandate_id`.
    pub async fn open_ticket(&self, open: TicketOpen<'_>) -> Result<String> {
        let TicketOpen {
            mandate_id,
            reply_address,
            turn_ref,
            created_at,
        } = open;
        let ticket_id = Uuid::now_v7().to_string();
        let id = ticket_id.clone();
        let mandate_id = mandate_id.to_string();
        let reply_address = reply_address.to_string();
        let turn_ref = turn_ref.map(str::to_string);
        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO admission_tickets \
                     (ticket_id, mandate_id, status, reply_address, turn_ref, created_at, updated_at) \
                     VALUES (?1, ?2, 'open', ?3, ?4, ?5, ?5)",
                    rusqlite::params![id, mandate_id, reply_address, turn_ref, created_at],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(ticket_id)
    }

    /// Close a live ticket with its report state (`ready|failed|dropped|stopped`).
    /// Returns `false` when the ticket was already ended (idempotent) or unknown.
    ///
    /// # Errors
    /// SQLite write failure.
    pub async fn close_ticket(&self, ticket_id: &str, report_state: &str) -> Result<bool> {
        let ticket_id = ticket_id.to_string();
        let report_state = report_state.to_string();
        let changed = self
            .client
            .writer()
            .call(move |conn| {
                Ok(conn.execute(
                    "UPDATE admission_tickets SET status = 'ended', report_state = ?2, \
                     updated_at = CAST(strftime('%s','now') AS INTEGER) \
                     WHERE ticket_id = ?1 AND status = 'open'",
                    rusqlite::params![ticket_id, report_state],
                )?)
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(changed == 1)
    }

    /// Ledger read path: filter by disposition and/or `created_at >= since`,
    /// newest first, bounded by `limit`.
    ///
    /// # Errors
    /// SQLite read failure.
    pub async fn list_decisions(
        &self,
        filter: DispositionFilter,
        limit: i64,
    ) -> Result<Vec<DecisionRow>> {
        let DispositionFilter { disposition, since } = filter;
        Ok(sqlx::query_as::<_, DecisionRow>(
            "SELECT id, source, event_id, content_hash, sender, thread_key, content, disposition, \
                    matched_rule, scope, accept_mode, strength, policy_version, reason, created_at \
             FROM admission_decisions \
             WHERE (?1 IS NULL OR disposition = ?2) AND (?3 IS NULL OR created_at >= ?4) \
             ORDER BY created_at DESC, rowid DESC LIMIT ?5",
        )
        .bind(disposition.clone())
        .bind(disposition)
        .bind(since)
        .bind(since)
        .bind(limit)
        .fetch_all(self.client.pool())
        .await?)
    }

    /// One decision record by id.
    ///
    /// # Errors
    /// SQLite read failure.
    pub async fn get_decision(&self, id: &str) -> Result<Option<DecisionRow>> {
        Ok(sqlx::query_as::<_, DecisionRow>(
            "SELECT id, source, event_id, content_hash, sender, thread_key, content, disposition, \
                    matched_rule, scope, accept_mode, strength, policy_version, reason, created_at \
             FROM admission_decisions WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(self.client.pool())
        .await?)
    }

    /// One mandate row by id (the withdraw/immutability read seam).
    ///
    /// # Errors
    /// SQLite read failure.
    pub async fn get_mandate(&self, mandate_id: &str) -> Result<Option<MandateRow>> {
        Ok(sqlx::query_as::<_, MandateRow>(
            "SELECT mandate_id, decision_id, matched_rule, scope, strength, accept_mode, \
                    policy_version, granted_at, withdrawn_at, withdrawn_reason \
             FROM admission_mandates WHERE mandate_id = ?1",
        )
        .bind(mandate_id)
        .fetch_optional(self.client.pool())
        .await?)
    }

    /// Continuation check (FR-003): the live (`open`) ticket whose admitted
    /// decision carries this `thread_key`, newest first. `None` means the
    /// thread needs a fresh admission.
    ///
    /// # Errors
    /// SQLite read failure.
    pub async fn open_ticket_for_thread(&self, thread_key: &str) -> Result<Option<TicketRow>> {
        Ok(sqlx::query_as::<_, TicketRow>(
            "SELECT t.ticket_id, t.mandate_id, t.status, t.report_state, t.reply_address, \
                    t.turn_ref, t.created_at, t.updated_at \
             FROM admission_tickets t \
             JOIN admission_mandates m ON m.mandate_id = t.mandate_id \
             JOIN admission_decisions d ON d.id = m.decision_id \
             WHERE d.thread_key = ?1 AND t.status = 'open' \
             ORDER BY t.created_at DESC, t.rowid DESC LIMIT 1",
        )
        .bind(thread_key)
        .fetch_optional(self.client.pool())
        .await?)
    }
}
