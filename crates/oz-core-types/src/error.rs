use thiserror::Error;

#[derive(Error, Debug)]
pub enum AgentError {
    #[error("LLM error: {0}")]
    LlmError(#[from] LlmError),

    #[error("Tool error: {0}")]
    ToolError(#[from] ToolError),

    #[error("Config error: {0}")]
    ConfigError(#[from] ConfigError),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Serde error: {0}")]
    SerdeError(#[from] serde_json::Error),

    #[error("{0}")]
    Custom(String),
}

#[derive(Error, Debug)]
pub enum LlmError {
    #[error("HTTP error: {status} {body}")]
    HttpError { status: u16, body: String },

    #[error("Request failed: {0}")]
    RequestFailed(#[from] reqwest::Error),

    #[error("Stream error: {0}")]
    StreamError(String),

    #[error("All sessions failed")]
    AllSessionsFailed,

    #[error("Max retries exceeded: {0}")]
    MaxRetriesExceeded(String),

    #[error("{0}")]
    Custom(String),
}

impl LlmError {
    pub fn is_retryable(&self) -> bool {
        match self {
            LlmError::HttpError { status, .. } => *status > 400 && *status != 401 && *status != 403,
            LlmError::RequestFailed(_) => true,
            // Mid-stream failures may have already emitted output; the
            // provider layer must not silently re-send the full context.
            // These bubble to the agent loop, which owns turn-level retry
            // and backoff. Pre-output failures (connection, HTTP status)
            // remain cheap to retry here.
            LlmError::StreamError(_) => false,
            _ => false,
        }
    }
}

#[derive(Error, Debug, Clone)]
pub enum ToolError {
    #[error("Missing argument: {0}")]
    MissingArg(String),

    #[error("Unsupported type: {0}")]
    UnsupportedType(String),

    #[error("File not found: {0}")]
    FileNotFound(String),

    #[error("Patch failed: {0}")]
    PatchFailed(String),

    #[error("{0}")]
    Custom(String),
}

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Failed to load config: {0}")]
    LoadFailed(String),

    #[error("Invalid config: {0}")]
    Invalid(String),

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("{0}")]
    Custom(String),
}

impl From<String> for AgentError {
    fn from(s: String) -> Self {
        AgentError::Custom(s)
    }
}

impl From<&str> for AgentError {
    fn from(s: &str) -> Self {
        AgentError::Custom(s.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── LlmError::is_retryable() ──

    #[test]
    fn is_retryable_399_not_retryable() {
        let err = LlmError::HttpError {
            status: 399,
            body: "ok".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn is_retryable_400_not_retryable() {
        let err = LlmError::HttpError {
            status: 400,
            body: "bad request".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn is_retryable_401_not_retryable() {
        let err = LlmError::HttpError {
            status: 401,
            body: "unauthorized".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn is_retryable_403_not_retryable() {
        let err = LlmError::HttpError {
            status: 403,
            body: "forbidden".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn is_retryable_404_is_retryable() {
        let err = LlmError::HttpError {
            status: 404,
            body: "not found".into(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn is_retryable_429_is_retryable() {
        let err = LlmError::HttpError {
            status: 429,
            body: "too many requests".into(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn is_retryable_500_is_retryable() {
        let err = LlmError::HttpError {
            status: 500,
            body: "internal error".into(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn is_retryable_502_is_retryable() {
        let err = LlmError::HttpError {
            status: 502,
            body: "bad gateway".into(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn is_retryable_503_is_retryable() {
        let err = LlmError::HttpError {
            status: 503,
            body: "unavailable".into(),
        };
        assert!(err.is_retryable());
    }

    #[test]
    fn is_retryable_0_not_retryable() {
        let err = LlmError::HttpError {
            status: 0,
            body: "edge".into(),
        };
        assert!(!err.is_retryable());
    }

    #[test]
    fn is_retryable_stream_error_not_retryable() {
        let err = LlmError::StreamError("connection lost".into());
        assert!(!err.is_retryable());
    }

    #[test]
    fn is_retryable_all_sessions_failed_not_retryable() {
        let err = LlmError::AllSessionsFailed;
        assert!(!err.is_retryable());
    }

    #[test]
    fn is_retryable_max_retries_exceeded_not_retryable() {
        let err = LlmError::MaxRetriesExceeded("done".into());
        assert!(!err.is_retryable());
    }

    #[test]
    fn is_retryable_custom_not_retryable() {
        let err = LlmError::Custom("some error".into());
        assert!(!err.is_retryable());
    }

    // ── AgentError From impls ──

    #[test]
    fn agent_error_from_string() {
        let err: AgentError = "custom message".into();
        match err {
            AgentError::Custom(msg) => assert_eq!(msg, "custom message"),
            _ => panic!("expected Custom variant"),
        }
    }

    #[test]
    fn agent_error_from_str() {
        let err: AgentError = std::convert::From::from("hello".to_string());
        match err {
            AgentError::Custom(msg) => assert_eq!(msg, "hello"),
            _ => panic!("expected Custom variant"),
        }
    }

    #[test]
    fn agent_error_from_empty_string() {
        let err: AgentError = "".to_string().into();
        assert!(matches!(err, AgentError::Custom(s) if s.is_empty()));
    }

    #[test]
    fn agent_error_from_llm_error() {
        let llm_err = LlmError::AllSessionsFailed;
        let err: AgentError = llm_err.into();
        match err {
            AgentError::LlmError(_) => {}
            _ => panic!("expected LlmError variant"),
        }
    }

    #[test]
    fn agent_error_from_tool_error() {
        let tool_err = ToolError::MissingArg("path".into());
        let err: AgentError = tool_err.into();
        match err {
            AgentError::ToolError(_) => {}
            _ => panic!("expected ToolError variant"),
        }
    }

    #[test]
    fn agent_error_from_config_error() {
        let config_err = ConfigError::Invalid("bad key".into());
        let err: AgentError = config_err.into();
        match err {
            AgentError::ConfigError(_) => {}
            _ => panic!("expected ConfigError variant"),
        }
    }

    // ── ToolError variants ──

    #[test]
    fn tool_error_missing_arg() {
        let err = ToolError::MissingArg("query".into());
        assert!(err.to_string().contains("query"));
    }

    #[test]
    fn tool_error_clone() {
        let err = ToolError::MissingArg("x".into());
        let cloned = err.clone();
        assert_eq!(err.to_string(), cloned.to_string());
    }

    // ── ConfigError variants ──

    #[test]
    fn config_error_load_failed() {
        let err = ConfigError::LoadFailed("io error".into());
        assert!(err.to_string().contains("load"));
    }

    #[test]
    fn config_error_invalid() {
        let err = ConfigError::Invalid("wrong type".into());
        assert!(err.to_string().contains("Invalid"));
    }

    #[test]
    fn config_error_session_not_found() {
        let err = ConfigError::SessionNotFound("ses_xyz".into());
        assert!(err.to_string().contains("ses_xyz"));
    }

    #[test]
    fn config_error_custom() {
        let err = ConfigError::Custom("unexpected".into());
        assert_eq!(err.to_string(), "unexpected");
    }

    // ── LlmError display messages ──

    #[test]
    fn llm_error_http_error_display() {
        let err = LlmError::HttpError {
            status: 500,
            body: "server error".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("HTTP error"));
        assert!(msg.contains("500"));
        assert!(msg.contains("server error"));
    }

    #[test]
    fn llm_error_stream_error_display() {
        let err = LlmError::StreamError("broken pipe".into());
        assert!(err.to_string().contains("broken pipe"));
    }

    #[test]
    fn llm_error_custom_display() {
        let err = LlmError::Custom("timeout".into());
        assert_eq!(err.to_string(), "timeout");
    }

    // ── AgentError display ──

    #[test]
    fn agent_error_custom_display() {
        let err: AgentError = "my error".into();
        assert_eq!(err.to_string(), "my error");
    }

    #[test]
    fn agent_error_from_llm_display() {
        let err: AgentError = LlmError::AllSessionsFailed.into();
        assert!(err.to_string().contains("LLM error"));
    }

    #[test]
    fn agent_error_chain() {
        let tool_err = ToolError::FileNotFound("missing.txt".into());
        let agent_err: AgentError = tool_err.into();
        let msg = agent_err.to_string();
        assert!(msg.contains("Tool error"));
    }
}
