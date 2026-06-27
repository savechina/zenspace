---
id: occams-razor
type: mental-model
name: "Occam's Razor"
domain: reasoning
source: "William of Ockham (14th century)"
application: "Prefer the simplest explanation that fits the evidence; avoid introducing unnecessary complexity"
created_at: 2026-06-26T00:00:00Z
updated_at: 2026-06-26T00:00:00Z
---

# Occam's Razor

Occam's Razor states that among competing explanations that account for the same evidence, the one with the fewest assumptions is usually correct. This is not a law of nature — complex explanations are sometimes right. It is a heuristic: when you have two explanations and one requires a chain of unlikely assumptions while the other does not, bet on the simpler one. It will be right more often.

The razor is most valuable when you are tempted to construct elaborate theories to explain simple phenomena. A system is slow. Explanation A: the database has an unindexed query that runs on every request. Explanation B: a series of caching failures combined with network latency and a race condition in the connection pool. Explanation A is testable with one query. Explanation B requires three unlikely things to go wrong simultaneously. Occam's Razor says: check the index first.

The discipline is resisting the urge to over-explain. Humans are pattern-seeking animals. We see conspiracies where there is incompetence, intentional design where there is accident, and deep strategy where there is luck. Simpler explanations feel unsatisfying because they do not engage our narrative instincts. But satisfying is not the same as correct. When the simple explanation fits the evidence, it is almost always the right one.

## When to Apply

- When a bug or failure has multiple possible causes — test the simplest cause first
- When a theory requires many unlikely assumptions to be true — suspect the theory, not reality
- When you catch yourself building a complex explanation for a simple problem — step back and ask "what is the most obvious answer?"
- When someone proposes a solution with many moving parts — ask whether a simpler approach would achieve 80% of the result
- When you notice you are the only person who understands your explanation — it is probably too complex
