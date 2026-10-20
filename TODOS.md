# TODOS

> **CLOSED 2026-10-17** — every tracked item is resolved (all 21 sub-sections carried a RESOLVED / IMPLEMENTED / CLOSED / Completed marker; the 2026-10-17 final sweep confirmed `005-agentic-loop/tasks.md` at 111/111 `[X]` with 0 unchecked and re-ran the full gates: `bin/lint` 0, `bin/test` 3282 passed / 0 failed / 25 skipped).
>
> The closure summary now lives with the owning spec — `docs/specs/005-agentic-loop/spec.md` → **"Open Review Backlog Closure Record (2026-10-17)"** (one line per item, with the owning feature noted). Incident-level fix/verification detail lives in the review records: `docs/specs/005-agentic-loop/review-2026-10-17-daemon-hang-incident.md` (§9 要点 ledger) and `review-2026-10-11-phase31.md`.
>
> This ledger is intentionally empty. Add new open items below this header; re-paste the closure pointer when the ledger is next emptied.

## Open items

- [x] perf: tier-1 rg stdout 无上限 — 已修（2026-10-20 同日落地）：读取线程 1 MiB 帽到量即 kill rg，stderr 并行排空防死锁，输出 lossy 解码（帽可能切在多字节中间）；in-process 兜底的整文件读同帽。钉 `test_tier1_output_cap_bounds_burst_without_losing_results`。
- [x] perf: PPR `load_graph_core` 每次融合查询全量加载 — 已修（2026-10-20 同日落地）：`GraphCoreCache` 挂 `SqliteClient`（per-database，杜绝跨库串图），TTL 60s（写侧失效需枚举 ~12 个写方法、漏一个即永久陈旧；TTL 有界陈旧对 0.9 权重排名信号可接受）。钉 `graph_core_cache_does_not_leak_across_databases`。
