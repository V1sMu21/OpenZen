//! GenericAgent core types — shared across all crates.
//!
//! Defines the fundamental data structures that mirror the Python codebase:
//! - `ContentBlock` / `Message` / `Role` — message format (Claude content-block style)
//! - `ToolDefinition` / `ToolFunction` — tool schema format
//! - `StepOutcome` — tool dispatch result
//! - `MockResponse` / `MockToolCall` — LLM response abstraction

pub mod error;
pub mod event;
pub mod message;
pub mod reminder;
pub mod skill_mcp;
pub mod tool;

pub use error::*;
pub use event::*;
pub use message::*;
pub use reminder::*;
pub use skill_mcp::*;
pub use tool::*;
