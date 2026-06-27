---
id: inductive-feedback
type: anti-pattern
category: feedback
trigger: "Leading user toward a preset answer through suggestive questions"
avoidance: "Ask open-ended questions; let the user define the problem before offering solutions"
severity: med
created_at: 2026-06-26T00:00:00Z
updated_at: 2026-06-26T00:00:00Z
---

# Inductive Feedback

Inductive feedback occurs when you ask questions that presuppose a particular answer. Instead of discovering what the user actually needs, you lead them toward confirming what you already believe or what your system is designed to deliver. The feedback appears genuine — you are asking questions, listening to answers — but the structure of the inquiry has already determined the outcome. The user feels heard, but their actual needs have not been surfaced.

This pattern is dangerous because it produces confident wrong answers. You gather data that validates your existing approach, and that data feels real because the user did respond. But the responses were shaped by your framing. A question like "would you like feature X?" assumes feature X is relevant. A better question is "what problem are you trying to solve?" The difference is between selling and discovering. Inductive feedback disguises selling as discovery.

The counter-strategy is structural: before asking any question, remove the solution from the question. Instead of "do you want a search feature?" ask "what do you do when you cannot find something?" Instead of "should we add dark mode?" ask "what frustrates you about the current interface?" The user's answer to the open question may surprise you. That surprise is valuable data. The confirmation you get from the leading question is not.

## Warning Signs

- Your questions all assume a specific solution is on the table
- The user's answers consistently confirm your existing hypothesis
- You feel more confident after feedback sessions, not less
- You cannot recall a time when user feedback changed your mind
- Your questions have "or" clauses that both point toward your preferred option

## Counter-strategy

- Rewrite every question to remove the solution from the phrasing
- Before a feedback session, write down what you believe and explicitly try to disprove it
- After each session, note one thing that surprised you — if nothing did, your questions were too narrow
- Use a colleague to review your questions before sessions to catch leading language
- Track how often feedback changes your plan — if it never does, you are inducting, not learning
