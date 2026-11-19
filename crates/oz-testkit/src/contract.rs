//! G0 — contract miner.
//!
//! The contract is read from the live `ToolRegistry`, not scraped from source:
//! the registry is what the model actually receives, so it cannot drift from
//! the prompt. Descriptions are dropped (they change often and are not part of
//! the call contract); names, property types and `required` are kept.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::aliases;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Prop {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enumeration: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolContract {
    pub name: String,
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub properties: std::collections::BTreeMap<String, Prop>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Contract {
    pub version: u32,
    pub tools: Vec<ToolContract>,
}

impl Contract {
    pub fn mine() -> Self {
        let registry = oz_tools::registry::ToolRegistry::build_default();
        let mut tools: Vec<ToolContract> = registry
            .to_schema("en")
            .into_iter()
            .map(|d| {
                let params = d.function.parameters;
                let required = params
                    .get("required")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut properties = std::collections::BTreeMap::new();
                if let Some(props) = params.get("properties").and_then(|v| v.as_object()) {
                    for (k, v) in props {
                        let mut enumeration = Vec::new();
                        if let Some(e) = v.get("enum").and_then(|e| e.as_array()) {
                            enumeration = e
                                .iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect();
                        }
                        properties.insert(
                            k.clone(),
                            Prop {
                                kind: v.get("type").and_then(|t| t.as_str()).map(str::to_string),
                                enumeration,
                            },
                        );
                    }
                }
                ToolContract {
                    name: d.function.name,
                    required,
                    properties,
                }
            })
            .collect();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        Contract { version: 1, tools }
    }

    pub fn get(&self, name: &str) -> Option<&ToolContract> {
        self.tools.iter().find(|t| t.name == name)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut s = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        s.push('\n');
        std::fs::write(path, s).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Drift checks. A failure means the generator's assumptions no longer
    /// hold — exit 4, never silently skip.
    pub fn check(&self) -> Vec<String> {
        let mut errs = Vec::new();
        if self.tools.is_empty() {
            errs.push("contract has zero tools".into());
        }
        for t in &self.tools {
            for r in &t.required {
                if !t.properties.contains_key(r) {
                    errs.push(format!(
                        "{}: required `{r}` has no property definition",
                        t.name
                    ));
                }
            }
        }
        for entry in aliases::TOOL_ALIASES {
            let Some(tc) = self.get(entry.tool) else {
                errs.push(format!(
                    "{}: curated tool missing from the live registry",
                    entry.tool
                ));
                continue;
            };
            for g in entry.groups {
                if !tc.properties.is_empty() && !tc.properties.contains_key(g.canonical) {
                    errs.push(format!(
                        "{}: canonical `{}` is not in the tool schema ({:?})",
                        entry.tool,
                        g.canonical,
                        tc.properties.keys().collect::<Vec<_>>()
                    ));
                }
                if g.aliases.contains(&g.canonical) {
                    errs.push(format!(
                        "{}: alias list contains the canonical key `{}`",
                        entry.tool, g.canonical
                    ));
                }
            }
        }
        errs
    }

    /// Per-tool schema summary used by the parameter generator.
    pub fn property_type(&self, tool: &str, key: &str) -> Option<String> {
        self.get(tool)
            .and_then(|t| t.properties.get(key))
            .and_then(|p| p.kind.clone())
    }

    pub fn required(&self, tool: &str) -> Vec<String> {
        self.get(tool)
            .map(|t| t.required.clone())
            .unwrap_or_default()
    }

    pub fn fingerprint(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        let mut h = 1469598103934665603u64;
        for b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(1099511628211);
        }
        format!("{h:016x}")
    }
}
