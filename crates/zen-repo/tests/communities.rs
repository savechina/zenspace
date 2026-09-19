//! Pure-function tests for the deterministic Louvain partitioner (T141).
//!
//! PURPOSE: Verifies the partitioner is a pure function of the graph and the
//! resolution — deterministic across runs and insertion orders, correct on
//! canonical clique graphs, and safe on empty input. No database involved.

use std::collections::HashMap;

use zen_repo::{Adjacency, louvain_communities};

/// Build a symmetric undirected adjacency from `(a, b, weight)` edges.
fn adj(edges: &[(&str, &str, f64)]) -> Adjacency {
    let mut a: Adjacency = HashMap::new();
    for (x, y, w) in edges {
        *a.entry(x.to_string())
            .or_default()
            .entry(y.to_string())
            .or_insert(0.0) += w;
        *a.entry(y.to_string())
            .or_default()
            .entry(x.to_string())
            .or_insert(0.0) += w;
    }
    a
}

/// Two triangles joined by a single bridge edge — the canonical two-community
/// graph for Louvain.
fn two_clique_graph() -> Adjacency {
    adj(&[
        ("A", "B", 1.0),
        ("B", "C", 1.0),
        ("A", "C", 1.0),
        ("D", "E", 1.0),
        ("E", "F", 1.0),
        ("D", "F", 1.0),
        ("C", "D", 1.0),
    ])
}

#[test]
fn two_runs_over_the_same_graph_are_identical() {
    let g = two_clique_graph();
    let first = louvain_communities(&g, 1.0);
    let second = louvain_communities(&g, 1.0);
    assert_eq!(first, second, "two runs must produce byte-identical output");
}

#[test]
fn insertion_order_does_not_change_the_result() {
    // Same graph, edges inserted in a different order (HashMap iteration
    // order is nondeterministic, so this pins the sorted-node determinism).
    let g1 = adj(&[
        ("A", "B", 1.0),
        ("B", "C", 1.0),
        ("A", "C", 1.0),
        ("D", "E", 1.0),
        ("E", "F", 1.0),
        ("D", "F", 1.0),
        ("C", "D", 1.0),
    ]);
    let g2 = adj(&[
        ("C", "D", 1.0),
        ("F", "D", 1.0),
        ("F", "E", 1.0),
        ("E", "D", 1.0),
        ("C", "A", 1.0),
        ("C", "B", 1.0),
        ("B", "A", 1.0),
    ]);
    assert_eq!(
        louvain_communities(&g1, 1.0),
        louvain_communities(&g2, 1.0),
        "insertion order must not affect the partition"
    );
}

#[test]
fn two_clique_graph_yields_two_communities() {
    let comms = louvain_communities(&two_clique_graph(), 1.0);
    assert_eq!(comms.len(), 2, "two cliques joined by one bridge edge");
    for c in &comms {
        assert_eq!(c.members.len(), 3, "each community holds one clique");
    }
}

#[test]
fn single_clique_graph_yields_one_community() {
    let g = adj(&[("A", "B", 1.0), ("B", "C", 1.0), ("A", "C", 1.0)]);
    let comms = louvain_communities(&g, 1.0);
    assert_eq!(comms.len(), 1, "a single triangle is one community");
    assert_eq!(comms[0].members.len(), 3);
}

#[test]
fn empty_graph_yields_no_communities() {
    let g: Adjacency = HashMap::new();
    let comms = louvain_communities(&g, 1.0);
    assert!(
        comms.is_empty(),
        "empty graph must not panic and yields nothing"
    );
}

#[test]
fn resolution_trades_granularity() {
    // A path of 4 nodes: at low resolution everything merges into one
    // community; at high resolution the weak links split it apart.
    let g = adj(&[("A", "B", 1.0), ("B", "C", 1.0), ("C", "D", 1.0)]);
    let coarse = louvain_communities(&g, 0.5);
    let fine = louvain_communities(&g, 2.0);
    assert!(
        coarse.len() < fine.len(),
        "higher resolution must produce more communities ({} < {})",
        coarse.len(),
        fine.len()
    );
}

#[test]
fn members_carry_their_node_weight() {
    let g = two_clique_graph();
    let comms = louvain_communities(&g, 1.0);
    for c in &comms {
        for m in &c.members {
            assert!(
                m.weight > 0.0,
                "every member of a non-empty community has positive node weight"
            );
        }
    }
}
