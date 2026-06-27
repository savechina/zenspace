---
id: context-inconsistency
type: anti-pattern
category: feedback
trigger: "Filling in unprovided context to make a coherent but inaccurate narrative"
avoidance: "Distinguish between what was said and what you inferred; ask before assuming"
severity: med
created_at: 2026-06-26T00:00:00Z
updated_at: 2026-06-26T00:00:00Z
---

# Context Inconsistency

Context inconsistency occurs when you fill in gaps in what someone said with what you assume they meant. The human brain is a gap-filling machine — it takes partial information and constructs a coherent narrative, smoothing over missing pieces with assumptions. When a user says "I want faster loading," your brain fills in the gaps: faster than what? On what device? Under what conditions? For what use case? The gaps are real, but your brain does not notice them because it has already constructed a complete story.

This pattern is dangerous because the narrative feels complete. You do not experience the missing context as missing — you experience the filled-in version as what was said. When you act on that filled-in version, you may solve a problem the user does not have. The user said "faster loading." You optimized the initial page load. But the user meant faster search results on mobile. Your solution is technically correct and practically useless. You solved a problem you invented, not the problem that exists.

The counter-strategy is explicit gap identification. When someone provides incomplete information, resist the urge to fill in the gaps mentally. Instead, list the gaps out loud or in writing. "You said you want faster loading. I am assuming you mean the initial page load on desktop. Is that correct?" This feels tedious. It is the most efficient thing you can do. Five seconds of clarification saves hours of building the wrong thing.

## Warning Signs

- You feel confident about what someone wants without asking follow-up questions
- Your summary of a conversation does not match the other person's summary
- You build something that works perfectly but nobody uses
- You find yourself saying "I assumed you meant..." frequently
- The gap between what was requested and what was delivered is large

## Counter-strategy

- After any conversation that will influence a decision, write down what was said and what you inferred — then verify the inferences
- When you catch yourself assuming context, stop and ask the question explicitly
- Use the phrase "let me make sure I understand" before acting on incomplete information
- In written communication, reply with your understanding of the request before starting work
- When the delivered result does not match expectations, check whether the mismatch started with an unfilled gap
