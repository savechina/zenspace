#![allow(clippy::module_inception)]
pub mod entity;
pub mod graph_provider;
pub mod relationship;
pub mod self_node;
pub mod service;

pub use entity::Entity;
pub use entity::EntityData;
pub use entity::EntityType;
pub use graph_provider::EntityGraphAdapter;
pub use relationship::RelationType;
pub use relationship::Relationship;
pub use self_node::SelfNode;
pub use service::EntityService;
