//! Named profiles: one-shot switch of data root / default model / policy.
//!
//! Profiles live in `~/.openzen/profiles.toml`:
//! ```toml
//! [profiles.dev]
//! data_dir = "/tmp/openzen-dev"
//! default_model = "local/omlx"
//! permissions = "permissions.dev.toml"
//! ```
//! Resolution order: `--profile NAME` > `OPENZEN_PROFILE` > `prod`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One `[profiles.NAME]` entry.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProfileEntry {
    #[serde(default)]
    pub data_dir: Option<PathBuf>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub permissions: Option<PathBuf>,
}

/// Resolved active profile.
#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub name: String,
    pub data_dir: Option<PathBuf>,
    pub default_model: Option<String>,
    pub permission_file: Option<PathBuf>,
}

/// Root of `profiles.toml`.
#[derive(Debug, Default, Deserialize)]
struct ProfilesFile {
    #[serde(default)]
    profiles: HashMap<String, ProfileEntry>,
}

/// Resolve the active profile name: `--profile` flag > `OPENZEN_PROFILE` env > "prod".
pub fn resolve_profile_name(args: &[String], env_profile: Option<&str>) -> String {
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if let Some(v) = a.strip_prefix("--profile=") {
            return v.to_string();
        }
        if a == "--profile" {
            if let Some(v) = iter.next() {
                return v.clone();
            }
        }
    }
    env_profile.map(|s| s.to_string()).unwrap_or_else(|| "prod".to_string())
}

/// Load a named profile from `<dir>/profiles.toml`; missing/parse error → defaults.
pub fn load_profile_from(dir: &Path, name: &str) -> Profile {
    let file: Option<ProfilesFile> = std::fs::read_to_string(dir.join("profiles.toml"))
        .ok()
        .and_then(|data| toml::from_str(&data).ok());
    match file.and_then(|f| f.profiles.get(name).cloned()) {
        Some(entry) => Profile {
            name: name.to_string(),
            data_dir: entry.data_dir,
            default_model: entry.default_model,
            permission_file: entry.permissions,
        },
        None => Profile { name: name.to_string(), ..Default::default() },
    }
}

/// Load the active profile from the default `~/.openzen/profiles.toml`.
pub fn load_profile() -> Profile {
    let name = resolve_profile_name(&std::env::args().skip(1).collect::<Vec<_>>(), std::env::var("OPENZEN_PROFILE").ok().as_deref());
    let dir = home_dir().join(".openzen");
    load_profile_from(&dir, &name)
}

fn home_dir() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("oz-config-profile-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn test_resolve_flag_beats_env_and_default() {
        assert_eq!(resolve_profile_name(&["--profile".into(), "dev".into()], Some("prod")), "dev");
        assert_eq!(resolve_profile_name(&["--profile=qa".into()], Some("dev")), "qa");
    }

    #[test]
    fn test_resolve_env_fallback() {
        assert_eq!(resolve_profile_name(&[], Some("dev")), "dev");
        assert_eq!(resolve_profile_name(&[], None), "prod");
    }

    #[test]
    fn test_load_profile_parses_entry() {
        let dir = tmp_dir("parse");
        fs::write(
            dir.join("profiles.toml"),
            r#"
[profiles.dev]
data_dir = "/tmp/openzen-dev"
default_model = "local/omlx"
permissions = "permissions.dev.toml"
"#,
        )
        .unwrap();
        let p = load_profile_from(&dir, "dev");
        assert_eq!(p.name, "dev");
        assert_eq!(p.data_dir, Some(PathBuf::from("/tmp/openzen-dev")));
        assert_eq!(p.default_model.as_deref(), Some("local/omlx"));
        assert_eq!(p.permission_file, Some(PathBuf::from("permissions.dev.toml")));
    }

    #[test]
    fn test_load_profile_unknown_name_defaults() {
        let dir = tmp_dir("unknown");
        fs::write(dir.join("profiles.toml"), "[profiles.dev]\ndata_dir = \"/tmp/x\"\n").unwrap();
        let p = load_profile_from(&dir, "missing");
        assert_eq!(p.name, "missing");
        assert_eq!(p.data_dir, None);
        assert_eq!(p.default_model, None);
    }

    #[test]
    fn test_load_profile_missing_file_defaults() {
        let dir = tmp_dir("missing").join("does-not-exist");
        let p = load_profile_from(&dir, "dev");
        assert_eq!(p.name, "dev");
        assert!(p.data_dir.is_none());
    }

    #[test]
    fn test_load_profile_bad_toml_defaults() {
        let dir = tmp_dir("bad");
        fs::write(dir.join("profiles.toml"), "not toml [[[").unwrap();
        let p = load_profile_from(&dir, "dev");
        assert_eq!(p.name, "dev");
        assert!(p.data_dir.is_none());
    }
}
