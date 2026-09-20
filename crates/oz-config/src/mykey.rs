use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

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

    /// Per-model extra HTTP headers appended (as default headers) to every
    /// LLM request — escape hatch for gateway routing/auth quirks without a
    /// rebuild. TOML: `["name".extra_headers]` table.
    #[serde(default)]
    pub extra_headers: Option<HashMap<String, String>>,
    /// Provider id this entry borrows `apibase`/`apikey` from
    /// (`[providers.<id>]` in mykey.toml). Resolution happens in
    /// `from_file` — by the time callers see a SessionConfig the fields
    /// are already flattened, so the LLM layer stays provider-unaware.
    #[serde(default)]
    pub provider: Option<String>,
    /// Declared input modalities ("text"/"image"/"video"/"audio").
    /// Declarative metadata for the settings UI and model switcher;
    /// absent/None is treated as ["text"] by convention.
    #[serde(default)]
    pub modalities: Option<Vec<String>>,
    /// Stable conversation tag for provider session-routing headers
    /// (opencode.ai `x-opencode-session`). Wired from the OpenZen session
    /// id at session-construction sites, never parsed from TOML.
    #[serde(skip)]
    pub session_tag: Option<String>,
}

/// Default SessionConfig context window (serde fallback + UI form default).
pub const DEFAULT_CONTEXT_WIN: usize = 28000;

fn default_context_win() -> usize {
    DEFAULT_CONTEXT_WIN
}

/// Top-level mykey.toml keys that hold configuration, not session entries.
/// `from_file` skips them when collecting sessions; writers of the file
/// (settings panel) must reject model names that would collide with them.
pub const RESERVED_TOP_LEVEL_KEYS: [&str; 7] = [
    "default_session",
    "summary_model",
    "memory_backend",
    "erme_idle_interval_secs",
    "tui",
    "router",
    "providers",
];

/// Heuristic: does this table look like a model/session entry? Settings-side
/// code uses it to tell real entries from unrelated config sections that
/// happen to carry a `provider` key (web_search, platforms.*).
pub fn looks_like_session_entry(t: &toml::Table) -> bool {
    t.contains_key("model") || t.contains_key("apibase") || t.contains_key("context_win")
}

/// The `provider = "<id>"` reference of an entry table, if present.
pub fn provider_ref(t: &toml::Table) -> Option<&str> {
    t.get("provider").and_then(|v| v.as_str())
}

/// A named endpoint credential shared by multiple model entries:
/// `[providers.<id>]` holds `apibase` + optional `apikey`; sessions
/// reference it with `provider = "<id>"` instead of repeating both.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    /// Base URL shared by every session entry referencing this provider.
    pub apibase: String,
    /// Local servers often need no key; absent/empty is valid.
    #[serde(default)]
    pub apikey: String,
}

/// API mode for OpenAI-compatible endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ApiMode {
    #[default]
    ChatCompletions,
    Responses,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MyKeyConfig {
    pub default_session: Option<String>,
    /// Model to use for compression summaries. When set, overrides auto-detection.
    pub summary_model: Option<String>,
    /// Memory backend: "file" (legacy full-text fallback) or "erme"
    /// (semantic memory engine — the default).
    pub memory_backend: String,
    /// Idle interval (seconds) between ERME soul-reflection cycles.
    /// Default 300 (5 min). Only used when memory_backend = "erme".
    pub erme_idle_interval_secs: Option<u64>,
    #[serde(default)]
    pub tui: TuiConfig,
    #[serde(default)]
    pub router: RouterConfig,
    /// Named endpoint credentials, keyed by provider id.
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
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
        let raw: toml::Table =
            toml::from_str(&content).map_err(|e| anyhow::anyhow!("TOML parse error: {e}"))?;
        let default_session = raw
            .get("default_session")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let summary_model = raw
            .get("summary_model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let memory_backend = match raw.get("memory_backend") {
            Some(v) => v
                .as_str()
                .map(|s| s.trim().to_string())
                .filter(|s| s == "file" || s == "erme")
                // Unknown value: safe fallback — never silently enable a
                // mode the user did not ask for.
                .unwrap_or_else(|| "file".to_string()),
            // Missing key: the integrated semantic default.
            None => "erme".to_string(),
        };
        let erme_idle_interval_secs = raw
            .get("erme_idle_interval_secs")
            .and_then(|v| v.as_integer())
            // Filter on i64 BEFORE casting: a negative value would wrap to a
            // huge u64 and sleep the idle thread ~forever.
            .filter(|i| *i > 0)
            .map(|i| i as u64);
        let mut sessions = HashMap::new();
        let mut providers = HashMap::new();

        // Parse `[providers.<id>]` credential tables first — session
        // entries referencing them need the values injected before
        // SessionConfig deserialization (apibase/apikey are required
        // fields there).
        if let Some(pt) = raw.get("providers").and_then(|v| v.as_table()) {
            for (id, value) in pt {
                match value.clone().try_into::<ProviderConfig>() {
                    Ok(p) => {
                        providers.insert(id.clone(), p);
                    }
                    Err(_) => {
                        // Lenient, same policy as invalid session tables:
                        // skip, don't fail the whole config. Warn loudly —
                        // every entry referencing it will be dropped.
                        tracing::warn!(
                            "mykey.toml: [providers.{id}] is invalid (apibase required) — \
                             entries referencing it will be skipped"
                        );
                        continue;
                    }
                }
            }
        }

        // Inject provider credentials into a raw session table before
        // deserialization. Entry-level apibase/apikey win (only missing
        // keys are filled); an unknown provider id fills nothing, so the
        // entry fails to parse and is skipped like any invalid table.
        fn inject_provider_fields(
            value: &mut toml::Value,
            providers: &HashMap<String, ProviderConfig>,
        ) {
            let Some(table) = value.as_table_mut() else {
                return;
            };
            let Some(pid) = provider_ref(table).map(str::to_string) else {
                return;
            };
            let Some(p) = providers.get(&pid) else {
                // A typo'd/unknown provider id leaves the entry without
                // credentials, so SessionConfig parsing fails and the entry
                // silently disappears from every consumer — surface it. Only
                // for entry-shaped tables: unrelated config sections that
                // happen to carry a `provider` key are not session entries.
                if looks_like_session_entry(table) {
                    tracing::warn!(
                        "mykey.toml: entry references unknown provider '{pid}' — \
                         entry will be skipped until the provider exists"
                    );
                }
                return;
            };
            if !table.contains_key("apibase") {
                table.insert(
                    "apibase".to_string(),
                    toml::Value::String(p.apibase.clone()),
                );
            }
            if !table.contains_key("apikey") {
                table.insert(
                    "apikey".to_string(),
                    toml::Value::String(p.apikey.clone()),
                );
            }
        }

        // Walk raw table to collect session entries. Dotted section names
        // like [qwen3.6-27b] are parsed as nested tables by the TOML spec.
        // We flatten them: if a value is a non-table, it's skipped; if it's
        // a table that looks like a SessionConfig, it's collected directly;
        // if it's a table with sub-tables, we walk deeper.
        fn collect_sessions(
            table: &toml::Table,
            prefix: &str,
            providers: &HashMap<String, ProviderConfig>,
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
                    let mut val_clone = value.clone();
                    inject_provider_fields(&mut val_clone, providers);
                    match val_clone.try_into::<SessionConfig>() {
                        Ok(sess) => {
                            sessions.insert(full_key, sess);
                        }
                        Err(_) => {
                            // Not a SessionConfig — might be nested dotted keys
                            collect_sessions(sub, &full_key, providers, sessions)?;
                        }
                    }
                }
                // Skip non-table values (strings, ints, etc.)
            }
            Ok(())
        }

        for (key, value) in &raw {
            if RESERVED_TOP_LEVEL_KEYS.contains(&key.as_str()) {
                continue;
            }
            if !value.is_table() {
                continue;
            }
            let sub = value.as_table().unwrap();
            // Try direct parse first
            let mut val_clone = value.clone();
            inject_provider_fields(&mut val_clone, &providers);
            match val_clone.try_into::<SessionConfig>() {
                Ok(sess) => {
                    sessions.insert(key.clone(), sess);
                }
                Err(_) => {
                    // Might be nested dotted-key structure
                    collect_sessions(sub, key, &providers, &mut sessions)?;
                }
            }
        }

        Ok(MyKeyConfig {
            default_session,
            summary_model,
            memory_backend,
            erme_idle_interval_secs,
            tui: TuiConfig::default(),
            router: RouterConfig::default(),
            providers,
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
            self.sessions
                .keys()
                .find(|k| !k.to_lowercase().contains("mixin"))
                .map(|s| s.as_str())
        })
    }

    pub fn iter_sessions(&self) -> impl Iterator<Item = (&String, &SessionConfig)> {
        self.sessions.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

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
        assert_eq!(
            SessionType::from_key_name("native_claude"),
            SessionType::NativeClaude
        );
    }

    #[test]
    fn session_type_native_oai() {
        assert_eq!(
            SessionType::from_key_name("native_oai"),
            SessionType::NativeOai
        );
    }

    #[test]
    fn session_type_oai_fallback() {
        assert_eq!(SessionType::from_key_name("gpt-4"), SessionType::Oai);
    }

    #[test]
    fn session_type_case_insensitive() {
        assert_eq!(SessionType::from_key_name("CLAUDE"), SessionType::Claude);
        assert_eq!(SessionType::from_key_name("MiXin_Key"), SessionType::Mixin);
        assert_eq!(
            SessionType::from_key_name("GPT-3.5-TURBO"),
            SessionType::Oai
        );
    }

    #[test]
    fn session_type_substring_matching() {
        assert_eq!(
            SessionType::from_key_name("my_claude_key"),
            SessionType::Claude
        );
        assert_eq!(
            SessionType::from_key_name("native_claude_prod"),
            SessionType::NativeClaude
        );
        assert_eq!(
            SessionType::from_key_name("native_oai_staging"),
            SessionType::NativeOai
        );
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
        )
        .unwrap();
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
        )
        .unwrap();
        let mode: ApiMode = cfg.api_mode;
        assert_eq!(mode, ApiMode::ChatCompletions);
    }

    #[test]
    fn session_config_extra_headers_and_no_session_tag_from_toml() {
        let cfg: SessionConfig = toml::from_str(
            r#"
            apikey = "sk-test"
            apibase = "https://opencode.ai/zen/go/v1"
            model = "glm-5.3-flash"
            extra_headers = { "x-opencode-session" = "sess-1" }
            "#,
        )
        .unwrap();
        let extra = cfg.extra_headers.expect("extra_headers parsed from TOML");
        assert_eq!(
            extra.get("x-opencode-session").map(String::as_str),
            Some("sess-1")
        );
        // session_tag is runtime-wired only, never parsed from TOML.
        assert!(cfg.session_tag.is_none());
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
        )
        .unwrap();
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
        )
        .unwrap();
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
        )
        .unwrap();
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

        fs::write(
            &path,
            r#"
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
"#,
        )
        .unwrap();

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
        fs::write(
            &path, r#"
"#,
        )
        .unwrap();

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

        fs::write(
            &path,
            r#"
default_session = "gpt4"

[gpt4]
apikey = "sk-gpt"
apibase = "https://api.openai.com/v1"
model = "gpt-4"

[claude]
apikey = "sk-claude"
apibase = "https://api.anthropic.com/v1"
model = "claude-3"
"#,
        )
        .unwrap();

        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert_eq!(cfg.default_session_name(), Some("gpt4"));

        fs::remove_dir_all(&tmp_dir).unwrap();
    }

    #[test]
    fn mykey_config_default_session_fallback_to_non_mixin() {
        let tmp_dir = env::temp_dir().join("oz_config_test_fallback");
        fs::create_dir_all(&tmp_dir).unwrap();
        let path = tmp_dir.join("fallback.toml");

        fs::write(
            &path,
            r#"
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
"#,
        )
        .unwrap();

        let cfg = MyKeyConfig::from_file(&path).unwrap();
        // No default_session set — should pick first non-mixin
        // HashMap iteration is not ordered, but result must be non-mixin and present
        let name = cfg.default_session_name();
        assert!(
            name.is_some(),
            "Should fall back to first non-mixin session"
        );
        let name = name.unwrap();
        assert!(
            !name.to_lowercase().contains("mixin"),
            "Fallback should skip mixin sessions"
        );
        assert!(
            cfg.get(name).is_some(),
            "Fallback session must exist in sessions map"
        );

        fs::remove_dir_all(&tmp_dir).unwrap();
    }

    #[test]
    fn mykey_config_default_session_all_mixin() {
        let tmp_dir = env::temp_dir().join("oz_config_test_all_mixin");
        fs::create_dir_all(&tmp_dir).unwrap();
        let path = tmp_dir.join("all_mixin.toml");

        fs::write(
            &path,
            r#"
[mixin_1]
apikey = "sk-mixin1"
apibase = "https://api.mixin.example/v1"
model = "mixin-llm"

[mixin_2]
apikey = "sk-mixin2"
apibase = "https://api.mixin.example/v1"
model = "mixin-llm"
"#,
        )
        .unwrap();

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
            memory_backend: "erme".to_string(),
            erme_idle_interval_secs: None,
            tui: TuiConfig::default(),
            router: RouterConfig::default(),
            providers: HashMap::new(),
        };
        assert_eq!(cfg.session_type("claude"), SessionType::Claude);
        assert_eq!(cfg.session_type("gpt-4"), SessionType::Oai);
    }

    #[test]
    fn mykey_config_iter_sessions() {
        let mut sessions = HashMap::new();
        sessions.insert(
            "gpt".to_string(),
            toml::from_str(
                r#"
            apikey = "sk-gpt"
            apibase = "https://api.openai.com/v1"
            model = "gpt-4"
            "#,
            )
            .unwrap(),
        );

        let cfg = MyKeyConfig {
            sessions,
            default_session: None,
            summary_model: None,
            memory_backend: "erme".to_string(),
            erme_idle_interval_secs: None,
            tui: TuiConfig::default(),
            router: RouterConfig::default(),
            providers: HashMap::new(),
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
        )
        .unwrap();
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
        )
        .unwrap();
        assert_eq!(cfg.api_mode, ApiMode::ChatCompletions);
    }

    fn write_config(tmp_name: &str, body: &str) -> std::path::PathBuf {
        let tmp_dir = env::temp_dir().join(tmp_name);
        fs::create_dir_all(&tmp_dir).unwrap();
        let path = tmp_dir.join("config.toml");
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn memory_backend_defaults_to_erme() {
        let path = write_config(
            "oz_config_test_mb_default",
            r#"
default_session = "gpt4"

[gpt4]
apikey = "sk-test"
apibase = "https://api.example.com/v1"
model = "gpt-4"
"#,
        );
        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert_eq!(cfg.memory_backend, "erme");
        assert_eq!(cfg.erme_idle_interval_secs, None);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn memory_backend_parses_erme() {
        let path = write_config(
            "oz_config_test_mb_erme",
            r#"
memory_backend = "erme"
default_session = "gpt4"

[gpt4]
apikey = "sk-test"
apibase = "https://api.example.com/v1"
model = "gpt-4"
"#,
        );
        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert_eq!(cfg.memory_backend, "erme");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn memory_backend_unknown_value_falls_back_to_file() {
        let path = write_config(
            "oz_config_test_mb_unknown",
            r#"
memory_backend = "quantum"
default_session = "gpt4"

[gpt4]
apikey = "sk-test"
apibase = "https://api.example.com/v1"
model = "gpt-4"
"#,
        );
        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert_eq!(cfg.memory_backend, "file");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn memory_backend_whitespace_trimmed() {
        let path = write_config(
            "oz_config_test_mb_trim",
            r#"
memory_backend = "  erme  "
default_session = "gpt4"

[gpt4]
apikey = "sk-test"
apibase = "https://api.example.com/v1"
model = "gpt-4"
"#,
        );
        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert_eq!(cfg.memory_backend, "erme");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn erme_idle_interval_parses() {
        let path = write_config(
            "oz_config_test_erme_idle",
            r#"
erme_idle_interval_secs = 60
default_session = "gpt4"

[gpt4]
apikey = "sk-test"
apibase = "https://api.example.com/v1"
model = "gpt-4"
"#,
        );
        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert_eq!(cfg.erme_idle_interval_secs, Some(60));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn erme_idle_interval_zero_is_ignored() {
        let path = write_config(
            "oz_config_test_erme_idle_zero",
            r#"
erme_idle_interval_secs = 0
default_session = "gpt4"

[gpt4]
apikey = "sk-test"
apibase = "https://api.example.com/v1"
model = "gpt-4"
"#,
        );
        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert_eq!(
            cfg.erme_idle_interval_secs, None,
            "0 must fall back to default"
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn erme_idle_interval_negative_is_ignored() {
        let path = write_config(
            "oz_config_test_erme_idle_neg",
            r#"
erme_idle_interval_secs = -1
default_session = "gpt4"

[gpt4]
apikey = "sk-test"
apibase = "https://api.example.com/v1"
model = "gpt-4"
"#,
        );
        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert_eq!(
            cfg.erme_idle_interval_secs, None,
            "negative must not wrap to a huge u64"
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn providers_parsed_and_session_inherits_credentials() {
        let path = write_config(
            "oz_config_test_provider_inherit",
            r#"
[providers.local_omlx]
apibase = "http://127.0.0.1:8000/v1"
apikey = "sk-local"

[gpt4]
provider = "local_omlx"
model = "gpt-4"
context_win = 128000
"#,
        );
        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert_eq!(cfg.providers.len(), 1);
        assert_eq!(cfg.providers["local_omlx"].apibase, "http://127.0.0.1:8000/v1");
        let sess = cfg.get("gpt4").expect("provider-referencing entry must parse");
        assert_eq!(sess.apibase, "http://127.0.0.1:8000/v1");
        assert_eq!(sess.apikey, "sk-local");
        assert_eq!(sess.provider.as_deref(), Some("local_omlx"));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn provider_keyless_provider_defaults_to_empty_apikey() {
        let path = write_config(
            "oz_config_test_provider_keyless",
            r#"
[providers.lmstudio]
apibase = "http://127.0.0.1:1234/v1"

[gpt4]
provider = "lmstudio"
model = "qwen3"
"#,
        );
        let cfg = MyKeyConfig::from_file(&path).unwrap();
        let sess = cfg.get("gpt4").unwrap();
        assert_eq!(sess.apikey, "");
        assert_eq!(sess.apibase, "http://127.0.0.1:1234/v1");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn entry_level_apibase_overrides_provider() {
        let path = write_config(
            "oz_config_test_provider_override",
            r#"
[providers.p1]
apibase = "http://provider:8000/v1"
apikey = "sk-provider"

[gpt4]
provider = "p1"
apibase = "http://override:8000/v1"
model = "gpt-4"
"#,
        );
        let cfg = MyKeyConfig::from_file(&path).unwrap();
        let sess = cfg.get("gpt4").unwrap();
        assert_eq!(sess.apibase, "http://override:8000/v1");
        assert_eq!(sess.apikey, "sk-provider");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn legacy_inline_entries_still_parse_alongside_providers() {
        let path = write_config(
            "oz_config_test_provider_legacy",
            r#"
default_session = "legacy"

[providers.p1]
apibase = "http://p1:8000/v1"
apikey = "sk-p1"

[legacy]
apikey = "sk-inline"
apibase = "http://legacy:8000/v1"
model = "old-model"

[gpt4]
provider = "p1"
model = "gpt-4"
"#,
        );
        let cfg = MyKeyConfig::from_file(&path).unwrap();
        let legacy = cfg.get("legacy").unwrap();
        assert_eq!(legacy.apibase, "http://legacy:8000/v1");
        assert_eq!(legacy.provider, None);
        assert!(cfg.get("gpt4").is_some());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn unknown_provider_id_entry_is_skipped_leniently() {
        let path = write_config(
            "oz_config_test_provider_unknown",
            r#"
default_session = "ok"

[providers.p1]
apibase = "http://p1:8000/v1"

[ok]
apikey = "sk-ok"
apibase = "http://ok:8000/v1"
model = "m1"

[broken]
provider = "no_such_provider"
model = "m2"
"#,
        );
        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert!(cfg.get("ok").is_some());
        assert!(cfg.get("broken").is_none(), "unresolvable entry must be skipped, not fail the file");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn providers_table_is_not_parsed_as_session() {
        let path = write_config(
            "oz_config_test_provider_reserved",
            r#"
[providers.p1]
apibase = "http://p1:8000/v1"
apikey = "sk-p1"
"#,
        );
        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert!(
            !cfg.sessions.contains_key("providers"),
            "providers table must not leak into sessions"
        );
        assert!(cfg.sessions.is_empty());
        assert_eq!(cfg.providers.len(), 1);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn modalities_parsed_and_default_to_none() {
        let path = write_config(
            "oz_config_test_modalities",
            r#"
[gpt4]
apikey = "sk-test"
apibase = "https://api.example.com/v1"
model = "gpt-4o"
modalities = ["text", "image"]

[vlm]
apikey = "sk-test"
apibase = "https://api.example.com/v1"
model = "vlm-2"
modalities = ["text", "image", "video", "audio"]
"#,
        );
        let cfg = MyKeyConfig::from_file(&path).unwrap();
        assert_eq!(
            cfg.get("gpt4").unwrap().modalities,
            Some(vec!["text".to_string(), "image".to_string()])
        );
        assert_eq!(
            cfg.get("vlm").unwrap().modalities,
            Some(vec![
                "text".to_string(),
                "image".to_string(),
                "video".to_string(),
                "audio".to_string()
            ])
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn session_config_modalities_default_none() {
        let cfg: SessionConfig = toml::from_str(
            r#"
            apikey = "sk-test"
            apibase = "https://api.example.com/v1"
            model = "gpt-4"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.modalities, None);
        assert_eq!(cfg.provider, None);
    }
}
