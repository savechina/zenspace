use rig_compose::context::Evidence;
use rig_compose::{ContextItem, ContextSourceKind};

pub fn evidence_to_context_items(evidence: &[Evidence]) -> Vec<ContextItem> {
    evidence
        .iter()
        .enumerate()
        .filter_map(|(rank, ev)| {
            let text = ev
                .detail
                .get("content")
                .and_then(|v| v.as_str())
                .or_else(|| ev.detail.get("text").and_then(|v| v.as_str()))?;

            if text.is_empty() {
                return None;
            }

            let source_id = format!("evidence/{}/{}", ev.source_skill, ev.label);

            Some(
                ContextItem::new(ContextSourceKind::Memory, source_id, text)
                    .with_rank(rank)
                    .with_score(fallback_score(rank)),
            )
        })
        .collect()
}

fn fallback_score(rank: usize) -> f64 {
    let rank = u32::try_from(rank).unwrap_or(u32::MAX);
    1.0 / f64::from(rank.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_evidence_yields_no_items() {
        let items = evidence_to_context_items(&[]);
        assert!(items.is_empty());
    }

    #[test]
    fn evidence_with_content_produces_item() {
        let ev = Evidence::new("memory", "recall").with_detail(json!({ "content": "user likes rust" }));
        let items = evidence_to_context_items(&[ev]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "user likes rust");
        assert!(matches!(items[0].source, ContextSourceKind::Memory));
    }

    #[test]
    fn evidence_with_text_fallback() {
        let ev = Evidence::new("user-input", "query").with_detail(json!({ "text": "hello" }));
        let items = evidence_to_context_items(&[ev]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "hello");
    }

    #[test]
    fn empty_content_filtered_out() {
        let ev = Evidence::new("memory", "empty").with_detail(json!({ "content": "" }));
        let items = evidence_to_context_items(&[ev]);
        assert!(items.is_empty());
    }

    #[test]
    fn items_ranked_ascending() {
        let ev1 = Evidence::new("a", "first").with_detail(json!({ "content": "one" }));
        let ev2 = Evidence::new("b", "second").with_detail(json!({ "content": "two" }));
        let items = evidence_to_context_items(&[ev1, ev2]);
        assert_eq!(items[0].rank, 0);
        assert_eq!(items[1].rank, 1);
        assert!(items[0].score > items[1].score);
    }
}
