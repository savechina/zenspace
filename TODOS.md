# TODOS

> **CLOSED 2026-10-17** — every tracked item is resolved (all 21 sub-sections carried a RESOLVED / IMPLEMENTED / CLOSED / Completed marker; the 2026-10-17 final sweep confirmed `005-agentic-loop/tasks.md` at 111/111 `[X]` with 0 unchecked and re-ran the full gates: `bin/lint` 0, `bin/test` 3282 passed / 0 failed / 25 skipped).
>
> The closure summary now lives with the owning spec — `docs/specs/005-agentic-loop/spec.md` → **"Open Review Backlog Closure Record (2026-10-17)"** (one line per item, with the owning feature noted). Incident-level fix/verification detail lives in the review records: `docs/specs/005-agentic-loop/review-2026-10-17-daemon-hang-incident.md` (§9 要点 ledger) and `review-2026-10-11-phase31.md`.
>
> This ledger is intentionally empty. Add new open items below this header; re-paste the closure pointer when the ledger is next emptied.

## Open items

- [ ] perf: tier-1 rg stdout 无上限 — `--max-count limit` 只按文件封顶（zen-vault/src/search/mod.rs），大 vault 全局匹配爆发时整段 stdout 读进一个 String 再截断。修法形状：读取线程按字节帽读满即 kill rg。2026-10-20 评审后推迟（实际输出小；kill-on-limit 需重做等待线程）。
- [ ] perf: PPR `load_graph_core` 每次命中 seed 的融合查询全量加载 notions+edges（zen-repo/src/notions_repo.rs）并 20 轮迭代。修法形状：TTL/版本键缓存，distill 图写入侧失效。2026-10-20 评审后推迟（失效语义 vs 图写入正确性需设计）。
