---
id: historical-rewrite
type: anti-pattern
category: cognitive
trigger: "Using past N successes as sole basis for future decisions"
avoidance: "Examine whether the conditions that enabled past success still hold; update assumptions explicitly"
severity: med
created_at: 2026-06-26T00:00:00Z
updated_at: 2026-06-26T00:00:00Z
---

# Historical Rewrite

Historical rewrite is the tendency to use past success as proof that a future decision will also succeed. "We did it before and it worked" becomes the entire argument. The past is not just a record of outcomes — it is a record of conditions. Those conditions may or may not still be true. Using past success without examining the conditions that enabled it is like using last year's map for this year's terrain. The roads have moved.

This pattern is dangerous because it substitutes memory for analysis. Memory is selective and biased. You remember the successes more vividly than the near-misses. You remember the final outcome but not the specific circumstances that made it possible. When you say "we shipped feature X in two weeks last time," you forget that last time you had a smaller scope, a more experienced team, and no integration requirements. The two-week estimate feels grounded in experience. It is grounded in an incomplete memory of a different situation.

The counter-strategy is structured examination. Before using past success as evidence, write down three conditions that were true during the past success. Then check whether each condition is still true. If any key condition has changed, the past success is not directly transferable. You need to re-estimate based on current conditions, not historical memory. This is tedious. It is also the difference between an estimate and a guess.

## Warning Signs

- Your planning relies heavily on "last time we..." without examining what was different last time
- You resist re-estimating because "we have done this before"
- The conditions surrounding a past success have changed significantly but the estimate has not
- You cannot articulate what conditions made the past success possible
- You feel uncomfortable when someone asks "what has changed since then?"

## Counter-strategy

- Before reusing a past estimate, list the conditions that made the original estimate accurate
- For each condition, verify whether it still holds; if not, adjust the estimate explicitly
- When someone says "we did it before," ask "what was different last time?"
- Maintain a simple log of past projects with the conditions and outcomes, not just the outcomes
- When conditions change, treat the past as a data point with context, not a template to copy
