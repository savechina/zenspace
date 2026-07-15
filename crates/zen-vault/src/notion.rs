#![allow(clippy::module_inception)]
pub mod notion;
pub mod graph_provider;
pub mod relationship;
pub mod self_node;
pub mod service;

pub use notion::{Notion, NotionData, NotionKind};
pub use graph_provider::NotionGraphAdapter;
pub use relationship::{RelationKind, Relationship};
pub use self_node::SelfNode;
pub use service::NotionService;
