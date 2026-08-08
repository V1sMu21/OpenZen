//! # ga-skill-mcp — Skill / MCP Registry
//!
//! Unified registry for skills, SOPs, facts, and insights — the substrate that
//! the `skill_mcp_search` / `skill_mcp_list` / `skill_mcp_store` /
//! `skill_mcp_refine` tools expose to the agent.
//!
//! Replaces the old `ga-memory` crate with a unified `.skill_mcp/` directory.
//!
//! ## Directory layout
//!
//! ```text
//! .skill_mcp/
//! ├── skills/              # SKILL.md files (opencode-compatible)
//! │   └── {name}/
//! │       ├── SKILL.md
//! │       └── meta.toml
//! ├── sops/                # Standard Operating Procedures
//! │   └── {name}.md
//! ├── facts/               # L2 persistent global facts
//! │   └── global_mem.txt
//! ├── insights/            # L1 distilled insights
//! │   └── global_mem_insight.txt
//! └── sessions/            # L4 raw session archives
//!     └── session_{ts}.md
//! ```
//!
//! ## Key abstractions
//!
//! - [`SkillMcpStore`] — the unified repository that manages all skill/MCP artifacts
//! - [`Skill`] — a SKILL.md capability definition
//! - [`MetaStore`] — metadata persistence (meta.toml)
//! - [`Matcher`] — cross-type artifact matching for context injection

pub mod meta;
pub mod memory;
pub mod skill;
pub mod sop;
pub mod matcher;
pub mod store;
pub mod staleness;
pub mod migration;

pub use meta::MetaStore;
pub use memory::SkillMcpMemory;
pub use skill::{Skill, SkillManager};
pub use sop::SopManager;
pub use matcher::{Matcher, MatchConfig};
pub use store::SkillMcpStore;
pub use staleness::{StalenessChecker, StalenessConfig};

/// Default on-disk directory name for the skill/MCP registry.
pub const SKILL_MCP_DIR: &str = ".skill_mcp";

/// Error type for skill/MCP registry operations.
#[derive(Debug, thiserror::Error)]
pub enum SkillMcpError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Serialization error: {0}")]
    Serialize(String),

    #[error("Skill/MCP artifact not found: {0}")]
    NotFound(String),

    #[error("Invalid format: {0}")]
    InvalidFormat(String),
}
