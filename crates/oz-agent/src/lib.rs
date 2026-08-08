use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub temperature: Option<f64>,
    pub instructions: Option<String>,
    #[serde(default)]
    pub use_tools: Option<String>,
    #[serde(default)]
    pub documents: Vec<String>,
    #[serde(default)]
    pub variables: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Agent {
    pub name: String,
    pub config: AgentConfig,
}

impl Agent {
    pub fn load(name: &str, agents_dir: &Path) -> anyhow::Result<Self> {
        let path = agents_dir.join(name).join("config.yaml");
        let content = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read agent config at {}: {}", path.display(), e))?;
        let config: AgentConfig = serde_yaml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse {}: {}", path.display(), e))?;
        Ok(Agent {
            name: name.to_string(),
            config,
        })
    }

    pub fn list(agents_dir: &Path) -> anyhow::Result<Vec<String>> {
        if !agents_dir.exists() {
            return Ok(Vec::new());
        }
        let mut names = Vec::new();
        for entry in std::fs::read_dir(agents_dir)
            .map_err(|e| anyhow::anyhow!("Failed to read agents dir: {}", e))?
        {
            let entry = entry?;
            if entry.path().join("config.yaml").exists() {
                if let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn interpolate_instructions(&self, user_input: &str) -> String {
        let mut text = self
            .config
            .instructions
            .clone()
            .unwrap_or_default();
        text = text.replace("__INPUT__", user_input);
        for (k, v) in &self.config.variables {
            text = text.replace(&format!("__{}__", k.to_uppercase()), v);
        }
        text
    }

    pub fn tool_names(&self) -> Vec<&str> {
        self.config
            .use_tools
            .as_ref()
            .map(|s| s.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()).collect())
            .unwrap_or_default()
    }
}

pub fn agents_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home)
        .join(".openzen")
        .join("agents")
}
