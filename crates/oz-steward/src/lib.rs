//! Steward: flight recorder, deterministic supervision and the local
//! decision layer.
//!
//! The crate is deliberately independent of `oz-core`: it consumes the plain
//! `StreamEvent` type, writes to SQLite, and talks to StartLux over HTTP.
//! Decision-making is local StartLux only; no hosted decision API.
//! That keeps the core loop free of steward dependencies and makes the
//! supervision layer testable without an LLM.

pub mod calibration;
pub mod contract;
pub mod decision;
pub mod detect;
pub mod error;
pub mod evidence;
pub mod goal;
pub mod intervene;
pub mod judge;
pub mod license;
pub mod llm_judge;
pub mod recorder;
pub mod report;
pub mod rollback;
pub mod startlux;
pub mod store;
pub mod supervise;
pub mod watchdog;

pub use contract::{Acceptance, ContractBudget, RiskLevel, TaskContract};
pub use decision::{Answer, DecisionProvider, DecisionRequest, DecisionResponse, Question};
pub use detect::{detect_all, Finding, FindingKind, Observation, Severity};
pub use error::{Result, StewardError};
pub use intervene::{CorrectionAction, CorrectionEngine, CorrectionLevel, CorrectionPlan};
pub use judge::{deterministic, max_verdict, nudge_text, Action, DeterministicJudgment};
pub use llm_judge::{JudgeOutcome, LLMJudge};
pub use recorder::Recorder;
pub use report::{GateReport, Report, ReportComposer, ReportStatus};
pub use rollback::{FileJournal, RollbackReport};
pub use startlux::{StartLuxConfig, StartLuxProvider, StartLuxTier, StartLuxVariant};
pub use store::Store;
pub use supervise::Supervisor;
pub use watchdog::{Deadman, Watchdog, WatchdogAction};
