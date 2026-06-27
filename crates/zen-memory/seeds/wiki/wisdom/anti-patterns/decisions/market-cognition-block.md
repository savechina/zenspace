---
id: market-cognition-block
type: anti-pattern
subtype: decision
category: decision
trigger: "Decision involves customer acquisition, traffic, or policy but lacks market environment data"
avoidance: "Research market conditions before deciding; use data-driven customer analysis"
severity: high
created_at: 2026-06-27T00:00:00Z
updated_at: 2026-06-27T00:00:00Z
---

# 市场认知盲区 (Market Cognition Block)

Market cognition block is making decisions about customer acquisition, traffic strategy, or regulatory environment without gathering actual market data. The decision-maker relies on intuition, anecdote, or outdated assumptions about who the customers are, where they spend time, what they value, and what the competitive landscape looks like. The result is a strategy built on a fictional market — internally consistent, externally disconnected.

This pattern is the startup killer. The product is built for an imagined customer. The marketing targets a channel that the real customers don't use. The pricing assumes a willingness to pay that doesn't exist. Each of these decisions feels reasonable in the absence of data — because the decision-maker is reasoning from assumptions, not evidence. The cost of market research feels high (time, money, ego risk of being wrong) compared to the cost of just building. But the cost of building the wrong thing is always higher.

## How It Manifests

- Launching a product without talking to any potential customers
- Choosing a marketing channel based on personal preference rather than audience data
- Pricing based on cost-plus rather than willingness-to-pay research
- Expanding to a new market without understanding local regulations or culture
- Assuming a feature will drive adoption without any user research to validate it

## Warning Signs

- You cannot describe your customer's daily routine in specific detail
- Your market size estimate is a top-down calculation, not bottom-up from actual users
- You have not spoken to a potential customer in the last 30 days
- Competitive analysis is based on website visits, not actual product testing
- "I think customers want..." — think is not data

## Counter-strategy

- Before any market-facing decision, answer: Who? Where? What do they currently use? Why would they switch?
- Conduct at least 10 customer interviews before building — listen, don't pitch
- Use bottom-up market sizing: actual reachable users × conversion rate × price
- Test with the smallest possible investment before committing major resources
- Track customer acquisition cost (CAC) against lifetime value (LTV) — if CAC > LTV, the market doesn't support the model
- Monitor regulatory environment continuously, not just at launch

## Related Principles

- 第一性原理 (First Principles): what do you actually KNOW about this market vs. assume?
- 逆向思维 (Inversion): what would make customers NOT adopt this? Avoid that first.
