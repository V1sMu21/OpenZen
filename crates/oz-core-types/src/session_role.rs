//! Session role and model provenance.
//!
//! A butler session loads the same prompt as every other session (Erya) and
//! only differs by a first-turn brief and by owning a task graph. Model
//! provenance matters: a model the user picked must never be silently
//! replaced by the cloud-preference default.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionRole {
    #[default]
    Worker,
    Butler,
}

impl SessionRole {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionRole::Worker => "worker",
            SessionRole::Butler => "butler",
        }
    }

    pub fn is_butler(self) -> bool {
        self == SessionRole::Butler
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    /// Resolved from profile/default config, not chosen by the user.
    #[default]
    Default,
    /// The user picked this model in the UI.
    User,
    /// A per-task override.
    Task,
}

impl ModelSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelSource::Default => "default",
            ModelSource::User => "user",
            ModelSource::Task => "task",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_defaults_to_worker_and_roundtrips() {
        assert_eq!(SessionRole::default(), SessionRole::Worker);
        let json = serde_json::to_string(&SessionRole::Butler).unwrap();
        assert_eq!(json, "\"butler\"");
        assert_eq!(
            serde_json::from_str::<SessionRole>("\"butler\"").unwrap(),
            SessionRole::Butler
        );
    }

    #[test]
    fn missing_role_field_deserializes_as_worker() {
        // Old sessions.json entries have no role field.
        let entry: SessionRole = serde_json::from_str("null").unwrap_or_default();
        assert_eq!(entry, SessionRole::Worker);
    }
}
