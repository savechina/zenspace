//! Community summarization consumer (T141 part B).
//!
//! PURPOSE: Turn the persisted Louvain communities into Obsidian-compatible
//! wiki pages under `wiki/communities/`, one page per community above a
//! minimum size. This is the consumer that makes the T141 partitioner
//! (part A, `zen-repo`) non-dead: without it the `notion_communities`
//! tables would be write-only.
//! USAGE: `run_community_summarization(client, wiki_dir, resolution, min_size, computed_at)`
//! from the zen-loop worker's Stage 4b (between graph verify and reindex).
//! EXPECTED: Communities at/above `min_size` appear as
//! `wiki/communities/<slug>.md` with typed frontmatter and `[[wikilink]]`
//! members; smaller communities are skipped entirely. Recomputation over
//! the same graph rewrites the same files (slug is a pure function of the
//! sorted member list), never creating duplicates.
//! ERRORS: Any failure returns `Err`; the caller (zen-loop Stage 4b) is
//! fail-open and warns, continuing the cycle.

use std::path::Path;

use anyhow::{Context, Result};
use zen_repo::{Community, CommunityMember, NotionsRepo, SqliteClient};

use crate::tindy::ChangeDetector;
use crate::wiki::AtomicWikiWriter;

/// Outcome of one community summarization run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommunitySummaryReport {
    /// Communities persisted to state.db (all, regardless of size).
    pub communities_persisted: usize,
    /// Wiki pages written for communities at/above the minimum size.
    pub pages_written: usize,
}

/// Run the full T141 part B consumer: partition → persist → summarize.
///
/// The partition step reuses [`NotionsRepo::compute_communities`], which
/// builds the open-edge projection from the same valid-edge snapshot
/// (`load_graph_core`) shared by `pagerank`/`personalized_pagerank` — no
/// second traversal is written here. Persistence replaces the previous
/// run's rows for the same (algorithm, resolution), so the tables cannot
/// grow monotonically with each cycle.
///
/// # Errors
/// Returns `Err` if the graph cannot be loaded, the persist transaction
/// fails, or any qualifying page write fails.
pub async fn run_community_summarization(
    client: &SqliteClient,
    wiki_dir: &Path,
    resolution: f64,
    min_size: u32,
    computed_at: &str,
) -> Result<CommunitySummaryReport> {
    let repo = NotionsRepo::new(client);
    let communities = repo.compute_communities(resolution).await?;
    repo.replace_communities("louvain", resolution, &communities, computed_at)
        .await?;

    let writer = AtomicWikiWriter::new(wiki_dir);
    let mut pages_written = 0usize;
    for community in &communities {
        // Strictly above the minimum: communities at or below it are skipped
        // entirely (small pairs are noise and would flood the vault).
        if (community.members.len() as u32) <= min_size {
            continue;
        }
        let slug = community_slug(&community.members);
        let path = Path::new("communities").join(format!("{slug}.md"));
        let content = render_community_page(community);
        writer
            .write(&path, &content)
            .with_context(|| format!("write community page: {}", path.display()))?;
        pages_written += 1;
    }

    Ok(CommunitySummaryReport {
        communities_persisted: communities.len(),
        pages_written,
    })
}

/// Deterministic page slug derived from the sorted member list (T141
/// decision 3). Recomputation over the same community must update the same
/// file, never create a duplicate — so the slug is a pure function of the
/// membership. A short SHA-256 prefix of the sorted list disambiguates
/// member sets whose names slugify identically (e.g. `{A, B-C}` vs
/// `{A, B, C}` both joining to `a-b-c`).
fn community_slug(members: &[CommunityMember]) -> String {
    let mut names: Vec<&str> = members.iter().map(|m| m.entity_name.as_str()).collect();
    names.sort_unstable();
    let joined = names.join("-");
    let base = slugify(&joined);
    // Hash the NUL-separated sorted list (not the `-`-joined string): the
    // joined form collapses distinct member sets (`{A, B-C}` and `{A, B, C}`
    // both join to `a-b-c`), so hashing it would not disambiguate.
    let digest = ChangeDetector::compute_checksum(&names.join("\u{0}"));
    let suffix = &digest[..8];
    if base.is_empty() {
        format!("community-{suffix}")
    } else {
        format!("{base}-{suffix}")
    }
}

/// Render a community as an Obsidian-compatible wiki page: typed
/// frontmatter (OKF `type: community`) plus a member list of `[[wikilinks]]`.
fn render_community_page(community: &Community) -> String {
    let mut page = String::from("---\n");
    page.push_str("type: community\n");
    page.push_str(&format!("label: {}\n", community.label));
    page.push_str(&format!("size: {}\n", community.members.len()));
    page.push_str("---\n\n");
    page.push_str(&format!("# {}\n\n", community.label));
    page.push_str(&format!(
        "A community of {} related entities detected by Louvain community detection over the notion graph.\n\n",
        community.members.len()
    ));
    page.push_str("## Members\n\n");
    for member in &community.members {
        page.push_str(&format!("- [[{}]]\n", member.entity_name));
    }
    page
}

/// Lowercase, map every non-alphanumeric character to `-`, collapse runs,
/// trim edges (same shape as the other vault slugifiers).
fn slugify(raw: &str) -> String {
    let mut slug: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    slug.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn member(name: &str) -> CommunityMember {
        CommunityMember {
            entity_name: name.to_string(),
            weight: 1.0,
        }
    }

    fn community(members: &[&str]) -> Community {
        Community {
            id: "c0".to_string(),
            label: members[0].to_string(),
            members: members.iter().map(|m| member(m)).collect(),
        }
    }

    /// Seed a fully-connected clique of `n` entities into a fresh DB.
    async fn seed_clique(client: &SqliteClient, names: &[&str]) {
        let repo = NotionsRepo::new(client);
        let now = chrono::Utc::now().to_rfc3339();
        for name in names {
            repo.upsert_entity(name, name, "concept", &now, &now)
                .await
                .unwrap();
        }
        let mut edge = 0usize;
        for i in 0..names.len() {
            for j in (i + 1)..names.len() {
                repo.insert_relationship(&zen_repo::InsertRelationshipRequest {
                    id: &format!("e{}-{edge}", names[i]),
                    source_id: names[i],
                    target_id: names[j],
                    rel_type: "related_to",
                    confidence: 1.0,
                    source_note_ids: None,
                    created_at: &now,
                    description: None,
                    valid_from: None,
                    valid_until: None,
                    weight: Some(1.0),
                })
                .await
                .unwrap();
                edge += 1;
            }
        }
    }

    #[tokio::test]
    async fn community_above_threshold_yields_page_with_wikilinks() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("state.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        seed_clique(&client, &["Rust", "Cargo", "Tokio", "Axum"]).await;

        let wiki_dir = dir.path().join("wiki");
        let report =
            run_community_summarization(&client, &wiki_dir, 1.0, 3, "2026-09-19T00:00:00Z")
                .await
                .unwrap();

        assert_eq!(report.communities_persisted, 1);
        assert_eq!(report.pages_written, 1);

        let entries: Vec<_> = std::fs::read_dir(wiki_dir.join("communities"))
            .unwrap()
            .collect();
        assert_eq!(entries.len(), 1, "exactly one community page");
        let path = entries[0].as_ref().unwrap().path();
        let content = std::fs::read_to_string(&path).unwrap();

        assert!(content.starts_with("---\ntype: community\n"), "{content}");
        for name in ["Rust", "Cargo", "Tokio", "Axum"] {
            assert!(
                content.contains(&format!("[[{name}]]")),
                "page must link member {name}: {content}"
            );
        }
    }

    #[tokio::test]
    async fn community_at_or_below_threshold_yields_no_page() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("state.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        // A 3-clique sits exactly AT the minimum (3) and a pair sits below it
        // — both must be skipped ("at or below the minimum are skipped
        // entirely"), pinning the strict `>` boundary.
        seed_clique(&client, &["Alpha", "Beta", "Gamma"]).await;
        seed_clique(&client, &["Delta", "Epsilon"]).await;

        let wiki_dir = dir.path().join("wiki");
        let report =
            run_community_summarization(&client, &wiki_dir, 1.0, 3, "2026-09-19T00:00:00Z")
                .await
                .unwrap();

        assert_eq!(report.communities_persisted, 2);
        assert_eq!(report.pages_written, 0);
        assert!(
            !wiki_dir.join("communities").exists(),
            "no communities dir may be created when nothing qualifies"
        );
    }

    #[tokio::test]
    async fn page_slug_stable_across_recomputation() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("state.db");
        let client = SqliteClient::open(&db_path).await.unwrap();
        seed_clique(&client, &["Rust", "Cargo", "Tokio", "Axum"]).await;

        let wiki_dir = dir.path().join("wiki");
        run_community_summarization(&client, &wiki_dir, 1.0, 3, "t1")
            .await
            .unwrap();
        let first: Vec<_> = std::fs::read_dir(wiki_dir.join("communities"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();

        run_community_summarization(&client, &wiki_dir, 1.0, 3, "t2")
            .await
            .unwrap();
        let second: Vec<_> = std::fs::read_dir(wiki_dir.join("communities"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();

        assert_eq!(first, second, "recomputation must rewrite the same file");
        assert_eq!(first.len(), 1);
    }

    #[test]
    fn slug_is_deterministic_and_order_independent() {
        let a = community_slug(&[member("Rust"), member("Cargo"), member("Tokio")]);
        let b = community_slug(&[member("Tokio"), member("Rust"), member("Cargo")]);
        assert_eq!(a, b, "slug must not depend on input order");
        assert!(
            a.starts_with("cargo-rust-tokio-"),
            "readable sorted base: {a}"
        );
    }

    #[test]
    fn slug_disambiguates_colliding_member_sets() {
        let a = community_slug(&[member("A"), member("B-C")]);
        let b = community_slug(&[member("A"), member("B"), member("C")]);
        assert_ne!(a, b, "distinct member sets must not share a page file");
    }

    #[test]
    fn render_has_typed_frontmatter_and_wikilinks() {
        let c = community(&["Rust", "Cargo", "Tokio"]);
        let page = render_community_page(&c);
        assert!(page.starts_with("---\ntype: community\nlabel: Rust\nsize: 3\n---\n"));
        assert!(page.contains("[[Rust]]"));
        assert!(page.contains("[[Cargo]]"));
        assert!(page.contains("[[Tokio]]"));
    }
}
