//! Platform-independent logic: data model, grid layout, item rules, persistence.
pub mod ipc;
pub mod layout;
pub mod look;
pub mod model;
pub mod rules;
pub mod store;

pub use look::*;
pub use model::*;
