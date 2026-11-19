//! The orchestrator's Rust core (PRD v2 E9): agent registry, runtime policy
//! and the agent factory. The turn loop drives these directly; the python
//! sidecar keeps only the tool execution and pending-action halves until the
//! python side retires.

pub mod factory;
pub mod registry;
pub mod runtime_policy;

pub use factory::AgentFactory;
pub use registry::{AgentDefinition, AgentRegistry};
pub use runtime_policy::build_system_prompt;
