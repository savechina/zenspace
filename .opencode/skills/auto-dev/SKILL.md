---
name: auto-dev
description: Supervised automated feature development loop. Takes a task (text, hypothesis slug, or arena regression), implements it via an external agent CLI in an isolated git worktree, verifies with the repo's own gates, and reports ready-for-merge. The runner verifies and records only — it never merges, never touches the main checkout. Use when asked to auto-implement a feature, work through the RSI backlog, or run the outer dev loop.
license: MIT
---

# Auto-Dev (supervised outer loop)

Peripheral tooling for zen-agent development: **you orchestrate, external agents implement, repo gates verify, humans merge.** The zen codebase is Rust — this skill never edits code directly; all implementation happens inside `codex exec` (or hermes/pi when available) scoped to a throwaway worktree.

## 0. Preconditions (fail closed)

- `git status --porcelain` on the main checkout must be clean. If dirty, stop and report — never auto-stash user work.
- `cargo build -q -p zen` succeeds (baseline builds before any agent runs).
- Intake must be one of: task text, `vault/wiki/wisdom/hypotheses/<slug>.md`, or an arena regression (`logs/adversarial-*.json` loser / `devloop-regression-*` slug).

## 1. Intake → worktree

1. Derive `SLUG` (kebab-case, e.g. `arena-loss-bare-singletons`).
2. `git worktree add /tmp/zen-dev-$SLUG -b dev/$SLUG`
3. Create `/tmp/zen-dev-$SLUG.ATTEMPTS.md` (attempt log, dsh attempt semantics: every try recorded, only verified results count).

## 2. Implement (external agent, max 2 attempts)

Run inside the worktree only:

```bash
codex exec -C /tmp/zen-dev-$SLUG -s workspace-write --ephemeral \
  -o /tmp/zen-dev-$SLUG.last.md \
  "$TASK_PROMPT — repo conventions: surgical changes, no new deps, match existing patterns, Rust (no allow-attributes to silence lints). Minimal change only."
```

- `$TASK_PROMPT` = intake background + acceptance criteria + failing evidence (test log / arena margin).
- Timeout 1800s per attempt. Non-zero exit → record attempt, feed the failure log back once (`codex exec resume --last "..."`), then escalate.
- hermes: blocked on auth in this env (`hermes setup` first); pi/dsh: absent. When available, same worktree protocol applies.

## 3. Verify (mechanical, by you — never delegated)

In the worktree, in order, stop at first red:

1. `cargo test -p zen-vault --lib` + `cargo test -p zen-agents --lib` (or the task's named scope)
2. `bin/lint` (clippy `-D warnings` + fmt; run with cwd=worktree)
3. If `git diff --name-only` touches `crates/zen-vault/src/distill/`: `cargo test -p zen-vault --lib adversarial::` (arena judge v2 as regression gate)
4. Convergence check (dsh turn-stopping gate): same test failing twice in a row with no new signal → stop, do not retry blindly.

## 4. Report (never merge)

- **GREEN**: print `READY-FOR-MERGE dev/$SLUG` + diff stat + gate evidence (test counts, lint clean). Leave the worktree in place for human inspection. Human merges.
- **RED**: print `BLOCKED dev/$SLUG` + failing gate + attempt log path. Suggest staging a hypothesis (`arena-loss-*` / `devloop-regression-*` convention) so the RSI loop picks it up.
- Cleanup (`git worktree remove`) only after the human confirms merge. Never `git push`, never merge to main.

## Cost/latency notes

- Each attempt is 1–2 LLM calls plus full test compiles (~1–3 min). Budget: 2 attempts default.
- codex auth: `codex login` (local) / `CODEX_API_KEY` (CI). Never echo keys into logs.
- Arena live runs (`zen discover arena --external`) are manual-only (auth/cost/latency); the default verification here is in-repo contestants.
