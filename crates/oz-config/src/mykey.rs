use std::collections::HashMap;
use std::path::Path;
use serde::Deserialize;

/// Session type inferred from config key name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SessionType {
    Oai,
    Claude,
    NativeClaude,
    NativeOai,
    Mixin,
}

impl SessionType {
    pub fn from_key_name(name: &str) -> Self {
        let l = name.to_lowercase();
        if l.contains("mixin") {
            SessionType::Mixin
        } else if l.contains("native_claude") {
            SessionType::NativeClaude
        } else if l.contains("native_oai") {
            SessionType::NativeOai
        } else if l.contains("claude") {
            SessionType::Claude
        } else {
            SessionType::Oai
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionConfig {
    pub apikey: String,
    pub apibase: String,
    pub model: String,

    #[serde(default = "default_context_win")]
    pub context_win: usize,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,

    #[serde(default)]
    pub api_mode: ApiMode,

    pub reasoning_effort: Option<String>,
    pub max_retries: Option<u32>,
    pub proxy: Option<String>,
    pub verify: Option<bool>,
    pub timeout: Option<u64>,

    // Mixin-specific
    pub llm_nos: Option<Vec<usize>>,
    pub base_delay: Option<f64>,
    pub spring_back: Option<u64>,
}

fn default_context_win() -> usize {
    28000
}

/// API mode for OpenAI-compatible endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiMode {
    ChatCompletions,
    Responses,
}

impl Default for ApiMode {
    fn default() -> Self { ApiMode::ChatCompletions }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MyKeyConfig {
    pub default_session: Option<String>,
    /// Model to use for compression summaries. When set, overrides auto-detection.
    pub summary_model: Option<String>,
    #[serde(default)]
    pub tui: TuiConfig,
    #[serde(default)]
    pub router: RouterConfig,
    pub sessions: HashMap<String, SessionConfig>,
}

/// TUI appearance configuration. Optional; absent values fall
/// back to hard-coded defaults in `ga-tui`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TuiConfig {
    /// Template for the left side of the prompt. Supports
    /// `{model}`, `{session}`, `{tokens}`, etc. See
    /// `oz_tui::template::PromptTemplate` for the full grammar.
    pub left_prompt: Option<String>,
    /// Template for the right side of the prompt.
    pub right_prompt: Option<String>,
    /// Theme name: "dark" or "light". Affects colour palette.
    pub theme: Option<String>,
    /// Per-colour overrides when `theme` alone isn't enough.
    /// Each field is a CSS hex colour like `"#6B9BB5"`. Any
    /// absent field falls back to the selected theme's default.
    #[serde(default)]
    pub theme_overrides: TuiThemeOverrides,
}

/// Custom colour overrides for the TUI theme. Accepts CSS hex
/// colours. See `oz_tui::theme::Theme::from_config()` for mapping.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TuiThemeOverrides {
    /// User message text colour.
    pub user_fg: Option<String>,
    /// Agent reply text colour.
    pub agent_fg: Option<String>,
    /// Muted text (separators, timestamps).
    pub muted_fg: Option<String>,
    /// Accent / logo colour.
    pub accent_fg: Option<String>,
    /// Highlight / selection / command colour.
    pub highlight_fg: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RouterConfig {
    pub cheap_model: Option<String>,
    pub flagship_model: Option<String>,
    pub complexity_threshold_chars: Option<usize>,
    pub complexity_threshold_tools: Option<usize>,
    #[serde(default)]
    pub route_rules: Vec<RouteRule>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteRule {
    pub pattern: String,
    pub model: String,
}

impl MyKeyConfig {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, anyhow::Error> {
        let content = std::fs::read_to_string(path.as_ref())?;
        let raw: toml::Table = toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("TOML parse error: {e}"))?;
        let default_session = raw.get("default_session")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let summary_model = raw.get("summary_model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let mut sessions = HashMap::new();

        // Walk raw table to collect session entries. Dotted section names
        // like [qwen3.6-27b] are parsed as nested tables by the TOML spec.
        // We flatten them: if a value is a non-table, it's skipped; if it's
        // a table that looks like a SessionConfig, it's collected directly;
        // if it's a table with sub-tables, we walk deeper.
        fn collect_sessions(
            table: &toml::Table,
            prefix: &str,
            sessions: &mut HashMap<String, SessionConfig>,
        ) -> Result<(), anyhow::Error> {
            for (key, value) in table {
                if value.is_table() {
                    let sub = value.as_table().unwrap();
                    // Try to parse this table directly as a SessionConfig
                    let full_key = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    let val_clone = value.clone();
                    match val_clone.try_into::<SessionConfig>() {
                        Ok(sess) => {
                            sessions.insert(full_key, sess);
                        }
                        Err(_) => {
                            // Not a SessionConfig — might be nested dotted keys
                            collect_sessions(sub, &full_key, sessions)?;
                        }
                    }
                }
                // Skip non-table values (strings, ints, etc.)
            }
            Ok(())
        }

        for (key, value) in &raw {
            if key == "default_session" { continue; }
            if key == "summary_model" { continue; }
            if key == "tui" { continue; }
            if key == "router" { continue; }
            if !value.is_table() { continue; }
            let sub = value.as_table().unwrap();
            // Try direct parse first
            let val_clone = value.clone();
            match val_clone.try_into::<SessionConfig>() {
                Ok(sess) => {
                    sessions.insert(key.clone(), sess);
                }
                Err(_) => {
                    // Might be nested dotted-key structure
                    collect_sessions(sub, key, &mut sessions)?;
                }
            }
        }

        Ok(MyKeyConfig {
            default_session,
            summary_model,
            tui: TuiConfig::default(),
            router: RouterConfig::default(),
            sessions,
        })
    }

    pub fn session_type(&self, name: &str) -> SessionType {
        SessionType::from_key_name(name)
    }

    pub fn get(&self, name: &str) -> Option<&SessionConfig> {
        self.sessions.get(name)
    }

    pub fn default_session_name(&self) -> Option<&str> {
        self.default_session.as_deref().or_else(|| {
            // Pick first non-mixin session
            self.sessions.keys().find(|k| !k.to_lowercase().contains("mixin")).map(|s| s.as_str())
        })
    }

    pub fn iter_sessions(&self) -> impl Iterator<Item = (&String, &SessionConfig)> {
        self.sessions.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::env;

    #[test]
    fn session_type_claude() {
        assert_eq!(SessionType::from_key_name("claude"), SessionType::Claude);
    }

    #[test]
    fn session_type_mixin() {
        assert_eq!(SessionType::from_key_name("mixin"), SessionType::Mixin);
    }

    #[test]
    fn session_type_native_claude() {
        assert_eq!(SessionType::from_key_name("native_claude"), SessionType::NativeClaude);
    }

    #[test]
    fn session_type_native_oai() {
        assert_eq!(SessionType::from_key_name("native_oai"), SessionType::NativeOai);
    }

    #[test]
    fn session_type_oai_fallback() {
        assert_eq!(SessionType::from_key_name("gpt-4"), SessionType::Oai);
    }

    #[test]
    fn session_type_case_insensitive() {
        assert_eq!(SessionType::from_key_name("CLAUDE"), SessionType::Claude);
        assert_eq!(SessionType::from_key_name("MiXin_Key"), SessionType::Mixin);
        assert_eq!(SessionType::from_key_name("GPT-3.5-TURBO"), SessionType::Oai);
    }

    #[test]
    fn session_type_substring_matching() {
        assert_eq!(SessionType::from_key_name("my_claude_key"), SessionType::Claude);
        assert_eq!(SessionType::from_key_name("native_claude_prod"), SessionType::NativeClaude);
        assert_eq!(SessionType::from_key_name("native_oai_staging"), SessionType::NativeOai);
        assert_eq!(SessionType::from_key_name("mixin_prod"), SessionType::Mixin);
    }

    #[test]
    fn api_mode_default() {
        assert_eq!(ApiMode::default(), ApiMode::ChatCompletions);
    }

    #[test]
    fn api_mode_partial_eq() {
        assert_eq!(ApiMode::ChatCompletions, ApiMode::ChatCompletions);
        assert_eq!(ApiMode::Responses, ApiMode::Responses);
        assert_ne!(ApiMode::ChatCompletions, ApiMode::Responses);
    }

    #[test]
    fn session_config_default_context_win() {
        let cfg: SessionConfig = toml::from_str(
            r#"
            apikey = "sk-test"
            apibase = "https://api.example.com/v1"
            model = "gpt-4"
            "#,
        ).unwrap();
        assert_eq!(cfg.context_win, 28000);
    }

    #[test]
    fn session_config_default_api_mode() {
        let cfg: SessionConfig = toml::from_str(
            r#"
            apikey = "sk-test"
            apibase = "https://api.example.com/v1"
            model = "gpt-4"
            "#,
        ).unwrap();
        let mode: ApiMode = cfg.api_mode;
        assert_eq!(mode, ApiMode::ChatCompletions);
    }

    #[test]
    fn session_config_custom_context_win() {
        let cfg: SessionConfig = toml::from_str(
            r#"
            apikey = "sk-test"
            apibase = "https://api.example.com/v1"
            model = "gpt-4"
            context_win = 120000
            "#,
        ).unwrap();
        assert_eq!(cfg.context_win, 120000);
    }

    #[test]
    fn session_config_optional_fields_defaults() {
        let cfg: SessionConfig = toml::from_str(
            r#"
            apikey = "sk-test"
            apibase = "https://api.example.com/v1"
            model = "gpt-4"
            "#,
        ).unwrap();
        assert_eq!(cfg.max_tokens, None);
        assert_eq!(cfg.temperature, None);
        assert_eq!(cfg.reasoning_effort, None);
        assert_eq!(cfg.max_retries, None);
        assert_eq!(cfg.proxy, None);
        assert_eq!(cfg.verify, None);
        assert_eq!(cfg.timeout, None);
        assert_eq!(cfg.llm_nos, None);
        assert_eq!(cfg.base_delay, None);
        assert_eq!(cfg.spring_back, None);
    }

    #[test]
    fn session_config_with_all_fields() {
        let cfg: SessionConfig = toml::from_str(
            r#"
            apikey = "sk-123"
            apibase = "https://api.openai.com/v1"
            model = "gpt-4o"
            context_win = 64000
            max_tokens = 4096
            temperature = 0.7
            api_mode = "responses"
            reasoning_effort = "high"
            max_retries = 3
            proxy = "http://proxy:8080"
            verify = false
            timeout = 120
            llm_nos = [1, 2, 3]
            base_delay = 1.5
            spring_back = 60
            "#,
        ).unwrap();
        assert_eq!(cfg.apikey, "sk-123");
        assert_eq!(cfg.apibase, "https://api.openai.com/v1");
        assert_eq!(cfg.model, "gpt-4o");
        assert_eq!(cfg.context_win, 64000);
        assert_eq!(cfg.max_tokens, Some(4096));
        assert_eq!(cfg.temperature, Some(0.7));
        assert_eq!(cfg.api_mode, ApiMode::Responses);
        assert_eq!(cfg.reasoning_effort, Some("high".to_string()));
        assert_eq!(cfg.max_retries, Some(3));
        assert_eq!(cfg.proxy, Some("http://proxy:8080".to_string()));
        assert_eq!(cfg.verify, Some(false));
        assert_eq!(cfg.timeout, Some(120));
        assert_eq!(cfg.llm_nos, Some(vec![1, 2, 3]));
        assert_eq!(cfg.base_delay, Some(1.5));
        assert_eq!(cfg.spring_back, Some(60));
    }

    #[test]
    fn mykey_config_from_valid_toml() {
        let tmp_dir = env::temp_dir().join("oz_config_test_main");
        fs::create_dir_all(&tmp_dir).unwrap();
        let path = tmp_dir.join("config.toml");

        fs::write(&path, r#"
default_session = "gpt4"

[gpt4]
apikey = "sk-gpt4key"
apibase = "https://api.openai.com/v1"
model = "gpt-4"

[claude]
apikey = "sk-claudekey"
apibase = "https://api.anthropic.com/v1"
model = "claude-3-opus"

[mixin_prod]
apikey = "sk-mixinkey"
apibase = "https://api.mixin.example/v1"
model = "mixin-llm"
"#).unwrap();

        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert_eq!(cfg.sessions.len(), 3);
        assert!(cfg.get("gpt4").is_some());
        assert!(cfg.get("claude").is_some());
        assert!(cfg.get("mixin_prod").is_some());
        assert!(cfg.get("nonexistent").is_none());

        fs::remove_dir_all(&tmp_dir).unwrap();
    }

    #[test]
    fn mykey_config_from_invalid_file() {
        let tmp_dir = env::temp_dir().join("oz_config_test_missing");
        // Do NOT create the file — should fail
        let path = tmp_dir.join("does_not_exist.toml");
        let result = MyKeyConfig::from_file(&path);
        assert!(result.is_err());
    }

    #[test]
    fn mykey_config_from_invalid_toml() {
        let tmp_dir = env::temp_dir().join("oz_config_test_bad");
        fs::create_dir_all(&tmp_dir).unwrap();
        let path = tmp_dir.join("bad.toml");

        fs::write(&path, "this is not valid toml {{{{").unwrap();
        let result = MyKeyConfig::from_file(&path);
        assert!(result.is_err());

        fs::remove_dir_all(&tmp_dir).unwrap();
    }

    #[test]
    fn mykey_config_empty_sessions() {
        let tmp_dir = env::temp_dir().join("oz_config_test_empty");
        fs::create_dir_all(&tmp_dir).unwrap();
        let path = tmp_dir.join("empty.toml");

        // A TOML with nothing parseable as sessions
        fs::write(&path, r#"
"#).unwrap();

        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert!(cfg.sessions.is_empty());
        assert_eq!(cfg.default_session_name(), None);

        fs::remove_dir_all(&tmp_dir).unwrap();
    }

    #[test]
    fn mykey_config_default_session_explicit() {
        let tmp_dir = env::temp_dir().join("oz_config_test_default");
        fs::create_dir_all(&tmp_dir).unwrap();
        let path = tmp_dir.join("default_session.toml");

        fs::write(&path, r#"
default_session = "gpt4"

[gpt4]
apikey = "sk-gpt"
apibase = "https://api.openai.com/v1"
model = "gpt-4"

[claude]
apikey = "sk-claude"
apibase = "https://api.anthropic.com/v1"
model = "claude-3"
"#).unwrap();

        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert_eq!(cfg.default_session_name(), Some("gpt4"));

        fs::remove_dir_all(&tmp_dir).unwrap();
    }

    #[test]
    fn mykey_config_default_session_fallback_to_non_mixin() {
        let tmp_dir = env::temp_dir().join("oz_config_test_fallback");
        fs::create_dir_all(&tmp_dir).unwrap();
        let path = tmp_dir.join("fallback.toml");

        fs::write(&path, r#"
[mixin_prod]
apikey = "sk-mixin"
apibase = "https://api.mixin.example/v1"
model = "mixin-llm"

[gpt4]
apikey = "sk-gpt"
apibase = "https://api.openai.com/v1"
model = "gpt-4"

[claude]
apikey = "sk-claude"
apibase = "https://api.anthropic.com/v1"
model = "claude-3"
"#).unwrap();

        let cfg = MyKeyConfig::from_file(&path).unwrap();
        // No default_session set — should pick first non-mixin
        // HashMap iteration is not ordered, but result must be non-mixin and present
        let name = cfg.default_session_name();
        assert!(name.is_some(), "Should fall back to first non-mixin session");
        let name = name.unwrap();
        assert!(!name.to_lowercase().contains("mixin"), "Fallback should skip mixin sessions");
        assert!(cfg.get(name).is_some(), "Fallback session must exist in sessions map");

        fs::remove_dir_all(&tmp_dir).unwrap();
    }

    #[test]
    fn mykey_config_default_session_all_mixin() {
        let tmp_dir = env::temp_dir().join("oz_config_test_all_mixin");
        fs::create_dir_all(&tmp_dir).unwrap();
        let path = tmp_dir.join("all_mixin.toml");

        fs::write(&path, r#"
[mixin_1]
apikey = "sk-mixin1"
apibase = "https://api.mixin.example/v1"
model = "mixin-llm"

[mixin_2]
apikey = "sk-mixin2"
apibase = "https://api.mixin.example/v1"
model = "mixin-llm"
"#).unwrap();

        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert_eq!(cfg.default_session_name(), None);

        fs::remove_dir_all(&tmp_dir).unwrap();
    }

    #[test]
    fn mykey_config_session_type_delegation() {
        let cfg = MyKeyConfig {
            sessions: HashMap::new(),
            default_session: None,
            summary_model: None,
            tui: TuiConfig::default(),
            router: RouterConfig::default(),
        };
        assert_eq!(cfg.session_type("claude"), SessionType::Claude);
        assert_eq!(cfg.session_type("gpt-4"), SessionType::Oai);
    }

    #[test]
    fn mykey_config_iter_sessions() {
        let mut sessions = HashMap::new();
        sessions.insert("gpt".to_string(), toml::from_str(
            r#"
            apikey = "sk-gpt"
            apibase = "https://api.openai.com/v1"
            model = "gpt-4"
            "#,
        ).unwrap());

        let cfg = MyKeyConfig {
            sessions,
            default_session: None,
            summary_model: None,
            tui: TuiConfig::default(),
            router: RouterConfig::default(),
        };

        let keys: Vec<_> = cfg.iter_sessions().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0], "gpt");
    }

    #[test]
    fn session_config_minimal_valid() {
        // Only required fields should be enough to deserialize
        let cfg: SessionConfig = toml::from_str(
            r#"
            apikey = "x"
            apibase = "y"
            model = "z"
            "#,
        ).unwrap();
        assert_eq!(cfg.apikey, "x");
        assert_eq!(cfg.apibase, "y");
        assert_eq!(cfg.model, "z");
        assert_eq!(cfg.context_win, 28000);
    }

    #[test]
    fn session_config_missing_required_field() {
        // Missing `apikey` should fail deserialization
        let result: Result<SessionConfig, _> = toml::from_str(
            r#"
            apibase = "https://api.example.com/v1"
            model = "gpt-4"
            "#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn session_config_api_mode_chat_completions() {
        let cfg: SessionConfig = toml::from_str(
            r#"
            apikey = "sk-test"
            apibase = "https://api.example.com/v1"
            model = "gpt-4"
            api_mode = "chat_completions"
            "#,
        ).unwrap();
        assert_eq!(cfg.api_mode, ApiMode::ChatCompletions);
    }
}
