//! Deterministic Louvain community detection (T141).
//!
//! PURPOSE: Partition the undirected weighted projection of the notion graph
//! into communities via the Louvain method (Blondel et al., 2008).
//! USAGE: `louvain_communities(&adjacency, resolution)` — the repo wrapper
//! (`NotionsRepo::compute_communities`) builds the adjacency from open
//! relationship edges and delegates here.
//! EXPECTED: A `Vec<Community>` with stable ids (`c0`, `c1`, ...) assigned in
//! a deterministic order; two runs over the same graph produce byte-identical
//! output.
//! ERRORS: None — this is a pure function; empty graphs return an empty vec.
//!
//! # Determinism
//! Node names are sorted before the algorithm runs, candidate communities are
//! visited in sorted order, and ties are broken by name, so the output is a
//! pure function of the graph and the resolution.
//!
//! # Known limitations
//! Louvain has a *resolution limit* (Fortunato & Barthelemy, 2007): below a
//! certain size, structurally distinct communities may be merged, and the
//! greedy local-moving phase can produce internally disconnected communities.
//! This is a deliberate, documented trade-off — **Leiden** (Traag et al.,
//! 2019) fixes both defects (guaranteed connected communities + a
//! resolution-limit-free refinement) and is the named upgrade path; this
//! module does not claim Leiden-grade quality.

use std::collections::HashMap;

use crate::types::{Community, CommunityMember};

/// Undirected weighted adjacency: `node -> (neighbor -> edge weight)`.
///
/// The map must be symmetric (`adj[a][b] == adj[b][a]`); the repo wrapper
/// guarantees this by construction. Self-loops are permitted (they arise in
/// the aggregated graph) and are counted twice in the degree, per the
/// standard undirected convention.
pub type Adjacency = HashMap<String, HashMap<String, f64>>;

/// Cap on local-moving passes inside one Louvain level. Each pass either
/// improves the partition or stops; the cap is a termination safety net for
/// pathological inputs, not a quality knob.
const MAX_LOCAL_MOVING_PASSES: usize = 100;

/// Run deterministic Louvain over `adjacency` at the given resolution.
///
/// `resolution` (γ) trades granularity for cohesion: γ < 1 favors larger
/// communities, γ > 1 favors smaller ones. The default is 1.0. Values ≤ 0
/// are degenerate (no resolution penalty — everything merges) and are the
/// caller's responsibility to avoid.
pub fn louvain_communities(adjacency: &Adjacency, resolution: f64) -> Vec<Community> {
    let mut nodes: Vec<&String> = adjacency.keys().collect();
    nodes.sort();

    if nodes.is_empty() {
        return Vec::new();
    }

    // Weighted degree per node in the ORIGINAL graph (self-loops count twice).
    let mut degree: HashMap<String, f64> = HashMap::new();
    for (node, neighbors) in adjacency {
        let d = neighbors
            .iter()
            .map(|(nbr, w)| if nbr == node { 2.0 * w } else { *w })
            .sum::<f64>();
        degree.insert(node.clone(), d);
    }

    // Each current-graph node maps to the set of original nodes it represents.
    let mut members: HashMap<String, Vec<String>> = nodes
        .iter()
        .map(|n| ((*n).clone(), vec![(*n).clone()]))
        .collect();

    let mut current: Adjacency = adjacency.clone();

    loop {
        let community = louvain_pass(&current, resolution);
        let (next, name) = aggregate(&current, &community);
        if next.len() == current.len() {
            // No shrink: the phase-1 partition is final.
            let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
            for (node, comm) in &community {
                grouped
                    .entry(comm.clone())
                    .or_default()
                    .extend(members[node].iter().cloned());
            }
            return build_communities(&grouped, &degree);
        }
        // Fold members through the aggregation, keyed by the NEW node names
        // (`name` maps old community ids to the aggregated graph's nodes).
        let mut next_members: HashMap<String, Vec<String>> = HashMap::new();
        for (node, comm) in &community {
            next_members
                .entry(name[comm].clone())
                .or_default()
                .extend(members[node].iter().cloned());
        }
        members = next_members;
        current = next;
    }
}

/// One local-moving pass: repeatedly move each node to the neighboring
/// community that maximizes the modularity gain until no move improves.
///
/// Returns the community assignment (current-graph node -> community id).
fn louvain_pass(adj: &Adjacency, gamma: f64) -> HashMap<String, String> {
    let mut nodes: Vec<&String> = adj.keys().collect();
    nodes.sort();

    // Weighted degree per node (self-loops count twice).
    let mut k: HashMap<String, f64> = HashMap::new();
    for (node, neighbors) in adj {
        let d = neighbors
            .iter()
            .map(|(nbr, w)| if nbr == node { 2.0 * w } else { *w })
            .sum::<f64>();
        k.insert(node.clone(), d);
    }

    // Total edge weight: each undirected edge counted once.
    let m: f64 = k.values().sum::<f64>() / 2.0;
    if m <= 0.0 {
        // No edges: every node is its own community.
        return nodes.iter().map(|n| ((*n).clone(), (*n).clone())).collect();
    }

    // Each node starts in its own community.
    let mut community: HashMap<String, String> =
        nodes.iter().map(|n| ((*n).clone(), (*n).clone())).collect();
    // tot[c] = sum of weighted degrees of nodes in community c.
    let mut tot: HashMap<String, f64> = k.clone();

    for _ in 0..MAX_LOCAL_MOVING_PASSES {
        let mut improved = false;
        for node in &nodes {
            // k_i_in[c] = sum of edge weights from node to nodes in c.
            // Self-loops are excluded: they are internal to the node no
            // matter which community it joins, so they never contribute to
            // the modularity gain (they still count in the degree k_i).
            let mut k_i_in: HashMap<String, f64> = HashMap::new();
            for (nbr, w) in &adj[*node] {
                if nbr == *node {
                    continue;
                }
                let c = community[nbr].clone();
                *k_i_in.entry(c).or_insert(0.0) += *w;
            }

            let cur = community[*node].clone();
            let k_node = k[*node];
            // Remove node from its current community.
            *tot.get_mut(&cur).unwrap() -= k_node;

            // Best neighboring community by modularity gain (sorted for
            // determinism; strict `>` keeps the smallest id on ties).
            let mut candidates: Vec<&String> = k_i_in.keys().collect();
            candidates.sort();
            let mut best: Option<String> = None;
            let mut best_gain = 0.0_f64;
            for c in candidates {
                let gain = k_i_in[c] - gamma * tot[c] * k_node / (2.0 * m);
                if gain > best_gain {
                    best_gain = gain;
                    best = Some(c.clone());
                }
            }

            match best {
                Some(b) if b != cur => {
                    community.insert((*node).clone(), b.clone());
                    *tot.get_mut(&b).unwrap() += k_node;
                    improved = true;
                }
                _ => {
                    // Stay: put the node's degree back.
                    *tot.get_mut(&cur).unwrap() += k_node;
                }
            }
        }
        if !improved {
            break;
        }
    }

    community
}

/// Aggregate the graph: each community becomes a node, edge weights between
/// communities are summed, and internal edges become self-loops.
///
/// Returns the new adjacency plus the `old community id -> new node name` map,
/// which the caller needs to fold member sets through the aggregation.
fn aggregate(
    adj: &Adjacency,
    community: &HashMap<String, String>,
) -> (Adjacency, HashMap<String, String>) {
    // Deterministic new node names for communities.
    let mut comms: Vec<&String> = community.values().collect();
    comms.sort();
    comms.dedup();
    let name: HashMap<String, String> = comms
        .iter()
        .enumerate()
        .map(|(i, c)| ((*c).clone(), format!("c{i}")))
        .collect();

    let mut next: Adjacency = HashMap::new();
    let mut nodes: Vec<&String> = adj.keys().collect();
    nodes.sort();
    for node in &nodes {
        let ci = name[&community[*node]].clone();
        let mut neighbors: Vec<&String> = adj[*node].keys().collect();
        neighbors.sort();
        for nbr in neighbors {
            if nbr.as_str() <= node.as_str() {
                continue; // each undirected edge processed once
            }
            let cj = name[&community[nbr]].clone();
            let w = adj[*node][nbr];
            if ci == cj {
                *next
                    .entry(ci.clone())
                    .or_default()
                    .entry(ci.clone())
                    .or_insert(0.0) += w;
            } else {
                *next
                    .entry(ci.clone())
                    .or_default()
                    .entry(cj.clone())
                    .or_insert(0.0) += w;
                *next
                    .entry(cj.clone())
                    .or_default()
                    .entry(ci.clone())
                    .or_insert(0.0) += w;
            }
        }
    }
    // Every community is a node, even with no edges.
    for c in name.values() {
        next.entry(c.clone()).or_default();
    }
    (next, name)
}

/// Build the final `Community` values from the grouped members, in a stable
/// order: communities sorted by their smallest member name, ids `c0`, `c1`, ...
fn build_communities(
    grouped: &HashMap<String, Vec<String>>,
    degree: &HashMap<String, f64>,
) -> Vec<Community> {
    let mut comms: Vec<&Vec<String>> = grouped.values().collect();
    comms.sort_by(|a, b| a.iter().min().cmp(&b.iter().min()));
    comms
        .into_iter()
        .enumerate()
        .map(|(i, members)| {
            let mut members = members.clone();
            members.sort();
            let label = members
                .iter()
                .max_by(|a, b| {
                    let wa = degree.get(*a).copied().unwrap_or(0.0);
                    let wb = degree.get(*b).copied().unwrap_or(0.0);
                    wa.total_cmp(&wb).then_with(|| a.cmp(b))
                })
                .cloned()
                .unwrap_or_default();
            Community {
                id: format!("c{i}"),
                label,
                members: members
                    .iter()
                    .map(|m| CommunityMember {
                        entity_name: m.clone(),
                        weight: degree.get(m).copied().unwrap_or(0.0),
                    })
                    .collect(),
            }
        })
        .collect()
}
