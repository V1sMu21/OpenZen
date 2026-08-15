use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::ReflectModule;

/// A single auto-fetch source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchSource {
    pub id: String,
    pub name: String,
    pub url: String,
    /// Interval in seconds between fetches.
    pub interval_secs: u64,
    /// Optional output filename (saved to memory/auto_fetch/). Defaults to {name}.txt.
    pub output_file: Option<String>,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_fetch: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl FetchSource {
    pub fn new(name: &str, url: &str, interval_secs: u64) -> Self {
        FetchSource {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            url: url.to_string(),
            interval_secs,
            output_file: None,
            enabled: true,
            last_fetch: None,
            last_error: None,
        }
    }

    pub fn is_due(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        if !self.enabled {
            return false;
        }
        match self.last_fetch {
            None => true,
            Some(last) => {
                let elapsed = now - last;
                elapsed.num_seconds() as u64 >= self.interval_secs
            }
        }
    }
}

/// Auto-fetch module — periodically fetches URLs and saves results to memory.
///
/// Config file: <base_dir>/config/auto_fetch.json
/// Output dir:  <base_dir>/memory/auto_fetch/
pub struct AutoFetchModule {
    config_file: PathBuf,
    output_dir: PathBuf,
    sources: Vec<FetchSource>,
    client: reqwest::Client,
}

impl AutoFetchModule {
    pub fn new(base_dir: &Path) -> Self {
        let config_file = base_dir.join("config").join("auto_fetch.json");
        let output_dir = base_dir.join("memory").join("auto_fetch");
        let _ = std::fs::create_dir_all(&output_dir);

        let sources = if config_file.exists() {
            std::fs::read_to_string(&config_file)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        AutoFetchModule {
            config_file,
            output_dir,
            sources,
            client,
        }
    }

    /// Add a fetch source.
    pub fn add_source(&mut self, source: FetchSource) {
        self.sources.push(source);
        self.save_config();
    }

    /// Remove a source by ID.
    pub fn remove_source(&mut self, id: &str) -> bool {
        let len_before = self.sources.len();
        self.sources.retain(|s| s.id != id);
        if self.sources.len() != len_before {
            self.save_config();
            true
        } else {
            false
        }
    }

    pub fn sources(&self) -> &[FetchSource] {
        &self.sources
    }

    fn save_config(&self) {
        if let Ok(content) = serde_json::to_string_pretty(&self.sources) {
            if let Some(parent) = self.config_file.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&self.config_file, content);
        }
    }

    /// Fetch a single source and save the result.
    async fn fetch_and_save(&self, source: &FetchSource) -> Result<String, String> {
        let response = self
            .client
            .get(&source.url)
            .send()
            .await
            .map_err(|e| format!("HTTP error: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            return Err(format!("HTTP {status}"));
        }

        let body = response
            .text()
            .await
            .map_err(|e| format!("Body read error: {e}"))?;

        let filename = source
            .output_file
            .clone()
            .unwrap_or_else(|| format!("{}.txt", source.name));
        let output_path = self.output_dir.join(&filename);
        std::fs::write(&output_path, &body).map_err(|e| format!("Write error: {e}"))?;

        let bytes = body.len();
        let summary = format!(
            "[AUTO_FETCH] \"{}\" — fetched {} ({} bytes) → {}",
            source.name,
            source.url,
            bytes,
            output_path.display()
        );
        Ok(summary)
    }
}

#[async_trait]
impl ReflectModule for AutoFetchModule {
    fn name(&self) -> &'static str {
        "auto_fetch"
    }

    async fn check(&self) -> Option<String> {
        let now = chrono::Utc::now();
        for source in &self.sources {
            if source.is_due(now) {
                match self.fetch_and_save(source).await {
                    Ok(summary) => {
                        return Some(summary);
                    }
                    Err(e) => {
                        tracing::warn!("[auto_fetch] {} fetch failed: {e}", source.name);
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_fetch_source_new() {
        let s = FetchSource::new("test", "https://example.com", 3600);
        assert_eq!(s.name, "test");
        assert!(s.enabled);
        assert!(s.last_fetch.is_none());
    }

    #[test]
    fn test_fetch_source_is_due() {
        let s = FetchSource::new("test", "https://example.com", 3600);
        assert!(s.is_due(chrono::Utc::now()));
    }

    #[test]
    fn test_fetch_source_not_due_yet() {
        let mut s = FetchSource::new("test", "https://example.com", 3600);
        s.last_fetch = Some(chrono::Utc::now());
        assert!(!s.is_due(chrono::Utc::now()));
    }

    #[test]
    fn test_fetch_source_disabled() {
        let mut s = FetchSource::new("test", "https://example.com", 3600);
        s.enabled = false;
        assert!(!s.is_due(chrono::Utc::now()));
    }

    #[test]
    fn test_fetch_source_due_after_interval() {
        let mut s = FetchSource::new("test", "https://example.com", 1);
        let past = chrono::Utc::now() - chrono::Duration::seconds(2);
        s.last_fetch = Some(past);
        assert!(s.is_due(chrono::Utc::now()));
    }

    #[test]
    fn test_add_remove_source() {
        let dir = tempfile::tempdir().unwrap();
        let mut module = AutoFetchModule::new(dir.path());
        assert!(module.sources().is_empty());

        module.add_source(FetchSource::new("a", "https://a.com", 60));
        assert_eq!(module.sources().len(), 1);

        let id = module.sources()[0].id.clone();
        assert!(module.remove_source(&id));
        assert!(module.sources().is_empty());

        // Config file should exist after add
        assert!(module.config_file.exists());
    }

    #[test]
    fn test_load_saved_config() {
        let dir = tempfile::tempdir().unwrap();

        {
            let mut module = AutoFetchModule::new(dir.path());
            module.add_source(FetchSource::new("saved", "https://saved.com", 120));
        } // drop

        // Re-create — should load from file
        let module = AutoFetchModule::new(dir.path());
        assert_eq!(module.sources().len(), 1);
        assert_eq!(module.sources()[0].name, "saved");
    }

    #[tokio::test]
    async fn test_reflect_module_name() {
        let dir = tempfile::tempdir().unwrap();
        let module = AutoFetchModule::new(dir.path());
        assert_eq!(module.name(), "auto_fetch");
    }
}
