//! `openzen key` — store web-search API keys without leaking them.
//!
//! The key is read from the terminal with echo disabled (or piped via
//! `--stdin`), so it never appears in shell history, in `ps` output, or in an
//! agent transcript. It is written to the `[web_search]` section of the
//! `mykey.toml` that `web_search` actually reads (the same path resolution, see
//! `oz_tools::web_search::search_config_write_path`).
//!
//! The file is edited line-by-line rather than round-tripped through a TOML
//! serializer: `mykey.toml` holds every session entry and `[providers.*]` /
//! `[platforms.*]` table, and reformatting it would reorder sections and drop
//! comments.

use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use oz_tools::web_search::{search_api_key, search_config_write_path, SearchKey, SEARCH_ENGINES};

/// Engines that authenticate with an API key.
const KEYED_ENGINES: [&str; 3] = ["bocha", "tavily", "firecrawl"];

const MIN_KEY_LEN: usize = 8;

/// Handle `openzen key ...`.
pub async fn handle_key_command(action: &crate::KeyAction, working_dir: &Path) -> Result<()> {
    let working_dir = working_dir.to_string_lossy().to_string();
    match action {
        crate::KeyAction::Set {
            engine,
            stdin,
            no_verify,
            file,
        } => {
            let engine = normalize_engine(engine)?;
            let path = match file {
                Some(path) => path.clone(),
                None => search_config_write_path(&working_dir),
            };

            warn_if_env_shadowed(&engine);

            let key = if *stdin {
                read_key_from_stdin()?
            } else {
                prompt_hidden(&format!("Paste the {engine} API key (input hidden): "))?
            };
            validate_key(&engine, &key)?;

            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let toml_key = toml_key_for(&engine);
            let updated = upsert_section_value(&existing, "web_search", toml_key, &key);

            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(&path, updated)
                .with_context(|| format!("writing {}", path.display()))?;
            harden_permissions(&path);

            println!(
                "✅ {engine} key stored in {} as [web_search] {toml_key}",
                path.display()
            );
            println!("   value: {}  ({} chars)", mask(&key), key.len());
            println!("   permissions: 600 (owner read/write only)");
            println!("   web_search reads this file on every call — no restart needed.");

            if !no_verify {
                print!("   verifying against api … ");
                std::io::stdout().flush().ok();
                match oz_tools::web_search::probe_engine_with_key(&engine, Some(&key), &working_dir).await {
                    Ok(count) => println!("ok — live search returned {count} results"),
                    Err(err) => {
                        println!("FAILED\n   ⚠️  key stored, but the live check failed: {err}");
                        println!(
                            "      A 401/403 means the key is wrong; a timeout/network error means \
                             the key may still be fine."
                        );
                    }
                }
            }
            Ok(())
        }

        crate::KeyAction::List => {
            let path = search_config_write_path(&working_dir);
            println!("web_search keys (config: {})", path.display());
            for engine in KEYED_ENGINES {
                print_key_status(engine, &working_dir);
            }
            println!("\nengines without a key (keyless or opt-in):");
            for engine in SEARCH_ENGINES {
                if !KEYED_ENGINES.contains(&engine) {
                    println!("  {engine:<10} —");
                }
            }
            Ok(())
        }

        crate::KeyAction::Remove { engine, file } => {
            let engine = normalize_engine(engine)?;
            let path = match file {
                Some(path) => path.clone(),
                None => search_config_write_path(&working_dir),
            };
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let (updated, removed) = remove_section_value(&content, "web_search", toml_key_for(&engine));
            if !removed {
                println!(
                    "Nothing to remove: no [web_search] {} in {}",
                    toml_key_for(&engine),
                    path.display()
                );
                return Ok(());
            }
            std::fs::write(&path, updated)
                .with_context(|| format!("writing {}", path.display()))?;
            println!(
                "🗑️  Removed [web_search] {} from {}",
                toml_key_for(&engine),
                path.display()
            );
            println!("   Note: an environment variable (if set) still wins over the file.");
            Ok(())
        }
    }
}

fn normalize_engine(engine: &str) -> Result<String> {
    let engine = engine.trim().to_lowercase();
    if KEYED_ENGINES.contains(&engine.as_str()) {
        Ok(engine)
    } else {
        Err(anyhow!(
            "engine '{engine}' does not take an API key. Keyed engines: {}",
            KEYED_ENGINES.join(", ")
        ))
    }
}

fn toml_key_for(engine: &str) -> &'static str {
    match engine {
        "bocha" => "bocha_api_key",
        "tavily" => "tavily_api_key",
        "firecrawl" => "firecrawl_api_key",
        _ => "api_key",
    }
}

/// Env var read before the file — warn when the stored key would be shadowed.
fn warn_if_env_shadowed(engine: &str) {
    let env_name = match engine {
        "bocha" => "BOCHA_API_KEY",
        "tavily" => "TAVILY_API_KEY",
        "firecrawl" => "FIRECRAWL_API_KEY",
        _ => return,
    };
    if std::env::var(env_name).map(|v| !v.trim().is_empty()).unwrap_or(false) {
        println!(
            "⚠️  {env_name} is set in this environment and takes precedence over the file value."
        );
    }
}

/// Reject values that would corrupt the TOML (or look like a mis-paste).
fn validate_key(engine: &str, key: &str) -> Result<()> {
    if key.len() < MIN_KEY_LEN {
        return Err(anyhow!(
            "{engine} key looks too short ({} chars) — paste the raw key only",
            key.len()
        ));
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(anyhow!(
            "{engine} key contains unexpected characters — expected letters, digits, '-', '_' or '.'"
        ));
    }
    if key.contains(char::is_whitespace) {
        return Err(anyhow!("{engine} key contains whitespace"));
    }
    Ok(())
}

fn mask(key: &str) -> String {
    let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("••••{tail}")
}

fn read_key_from_stdin() -> Result<String> {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("reading key from stdin")?;
    Ok(line.trim().to_string())
}

/// Read a line from the terminal without echoing it.
fn prompt_hidden(prompt: &str) -> Result<String> {
    if !std::io::stdin().is_terminal() {
        return Err(anyhow!(
            "stdin is not a terminal — pipe the key instead: \
             printf '%s' \"$KEY\" | openzen key set <engine> --stdin"
        ));
    }

    eprint!("{prompt}");
    std::io::stderr().flush().ok();

    // RAII: echo is restored even if read_line fails.
    let guard = EchoOff::engage();
    if !guard.echo_disabled {
        eprintln!(
            "\n⚠️  could not disable terminal echo — the key will be visible as you type. \
             Prefer: printf '%s' \"$KEY\" | openzen key set <engine> --stdin"
        );
    }
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    drop(guard);
    eprintln!();
    Ok(line.trim().to_string())
}

#[cfg(unix)]
struct EchoOff {
    echo_disabled: bool,
}

#[cfg(unix)]
impl EchoOff {
    fn engage() -> Self {
        let echo_disabled = std::process::Command::new("stty")
            .arg("-echo")
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        Self { echo_disabled }
    }
}

#[cfg(unix)]
impl Drop for EchoOff {
    fn drop(&mut self) {
        let _ = std::process::Command::new("stty").arg("echo").status();
    }
}

#[cfg(not(unix))]
struct EchoOff {
    echo_disabled: bool,
}

#[cfg(not(unix))]
impl EchoOff {
    fn engage() -> Self {
        Self {
            echo_disabled: false,
        }
    }
}

/// Make the config owner-only; it holds API keys for every provider.
#[cfg(unix)]
fn harden_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn harden_permissions(_path: &Path) {}

fn print_key_status(engine: &str, working_dir: &str) {
    match search_api_key(engine, working_dir) {
        Some(SearchKey { key, source }) => {
            println!(
                "  {engine:<10} ✅ {}  ({}, {} chars)  from {source}",
                mask(&key),
                toml_key_for(engine),
                key.len()
            );
        }
        None => println!(
            "  {engine:<10} ❌ not configured  ({} / [web_search] {})",
            env_var_for(engine),
            toml_key_for(engine)
        ),
    }
}

fn env_var_for(engine: &str) -> &'static str {
    match engine {
        "bocha" => "BOCHA_API_KEY",
        "tavily" => "TAVILY_API_KEY",
        "firecrawl" => "FIRECRAWL_API_KEY",
        _ => "-",
    }
}

/* ------------------------------------------------------------------ *
 * Surgical `[section]` editing
 * ------------------------------------------------------------------ */

/// Line range of `[section]`: from its header to the next header (or EOF).
fn section_bounds(lines: &[&str], section: &str) -> Option<(usize, usize)> {
    let header = format!("[{section}]");
    let start = lines.iter().position(|line| line.trim() == header)?;
    let end = lines[start + 1..]
        .iter()
        .position(|line| {
            let trimmed = line.trim();
            trimmed.starts_with('[') && trimmed.ends_with(']')
        })
        .map(|offset| start + 1 + offset)
        .unwrap_or(lines.len());
    Some((start, end))
}

fn is_key_line(line: &str, key: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return false;
    }
    match trimmed.split_once('=') {
        Some((name, _)) => name.trim() == key,
        None => false,
    }
}

/// Set `key = "value"` inside `[section]`, preserving every other byte.
pub fn upsert_section_value(content: &str, section: &str, key: &str, value: &str) -> String {
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let entry = format!("{key} = \"{value}\"");
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();

    match section_bounds(&borrowed, section) {
        Some((start, end)) => {
            match lines[start + 1..end]
                .iter()
                .position(|line| is_key_line(line, key))
            {
                Some(offset) => lines[start + 1 + offset] = entry,
                None => {
                    // Append after the section's last content line, not after
                    // any blank lines that separate it from the next header.
                    let mut insert_at = end;
                    while insert_at > start + 1 && lines[insert_at - 1].trim().is_empty() {
                        insert_at -= 1;
                    }
                    lines.insert(insert_at, entry);
                }
            }
        }
        None => {
            if lines.iter().any(|line| !line.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("[{section}]"));
            lines.push(entry);
        }
    }

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Drop `key` from `[section]`; the bool reports whether a line was removed.
pub fn remove_section_value(content: &str, section: &str, key: &str) -> (String, bool) {
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut removed = false;

    if let Some((start, end)) = section_bounds(&borrowed, section) {
        if let Some(offset) = lines[start + 1..end]
            .iter()
            .position(|line| is_key_line(line, key))
        {
            lines.remove(start + 1 + offset);
            removed = true;
        }
    }

    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    (out, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
default_session = \"local\"

[local]
model = \"m\"

[web_search]
bocha_api_key = \"old-key\"

[platforms.telegram]
bot_token = \"t\"
";

    #[test]
    fn upsert_replaces_in_place_and_keeps_everything_else() {
        let updated = upsert_section_value(SAMPLE, "web_search", "tavily_api_key", "tv-123");
        assert!(updated.contains("bocha_api_key = \"old-key\""));
        assert!(updated.contains("[platforms.telegram]\nbot_token = \"t\""));
        assert!(updated.contains("bocha_api_key = \"old-key\"\ntavily_api_key = \"tv-123\"\n"));
        // Appended inside web_search, i.e. before the next section header.
        let tv_at = updated.find("tavily_api_key").unwrap();
        let tg_at = updated.find("[platforms.telegram]").unwrap();
        assert!(tv_at < tg_at);

        // Replacing an existing key must not duplicate it.
        let replaced = upsert_section_value(&updated, "web_search", "tavily_api_key", "tv-456");
        assert_eq!(replaced.matches("tavily_api_key").count(), 1);
        assert!(replaced.contains("tavily_api_key = \"tv-456\""));
    }

    #[test]
    fn upsert_creates_missing_section() {
        let updated = upsert_section_value("[local]\nmodel = \"m\"\n", "web_search", "tavily_api_key", "tv-1");
        assert!(updated.contains("[web_search]\ntavily_api_key = \"tv-1\"\n"));

        let from_empty = upsert_section_value("", "web_search", "tavily_api_key", "tv-1");
        assert_eq!(from_empty, "[web_search]\ntavily_api_key = \"tv-1\"\n");
    }

    #[test]
    fn remove_deletes_only_the_target_key() {
        let (updated, removed) = remove_section_value(SAMPLE, "web_search", "bocha_api_key");
        assert!(removed);
        assert!(!updated.contains("bocha_api_key"));
        assert!(updated.contains("[web_search]"));
        assert!(updated.contains("[platforms.telegram]"));

        let (untouched, removed_again) = remove_section_value(&updated, "web_search", "bocha_api_key");
        assert!(!removed_again);
        assert_eq!(untouched, updated);
    }

    #[test]
    fn key_line_detection_ignores_comments_and_other_sections() {
        assert!(is_key_line("bocha_api_key = \"x\"", "bocha_api_key"));
        assert!(is_key_line("  bocha_api_key=\"x\"", "bocha_api_key"));
        assert!(!is_key_line("# bocha_api_key = \"x\"", "bocha_api_key"));
        assert!(!is_key_line("bocha_api_key_other = \"x\"", "bocha_api_key"));
    }

    #[test]
    fn keys_are_validated_before_writing() {
        assert!(validate_key("tavily", "tvly-dev-abcdef123456").is_ok());
        assert!(validate_key("tavily", "short").is_err());
        assert!(validate_key("tavily", "has space in it here").is_err());
        assert!(validate_key("tavily", "quote\"injection").is_err());
        assert!(validate_key("tavily", "newline\ninjection").is_err());
    }

    #[test]
    fn mask_never_reveals_the_body() {
        assert_eq!(mask("tvly-dev-1234567890QAmP"), "••••QAmP");
        assert!(!mask("tvly-dev-1234567890QAmP").contains("1234567890"));
        assert_eq!(mask("ab"), "••••ab");
    }

    #[test]
    fn engine_names_are_normalized_and_gated() {
        assert_eq!(normalize_engine(" TAVILY ").unwrap(), "tavily");
        assert!(normalize_engine("bing-rss").is_err());
        assert!(normalize_engine("exa").is_err());
    }

    #[test]
    fn write_path_and_read_path_resolve_to_the_same_file() {
        // The reader walks `search_config_candidates` in order; the writer
        // targets the first existing candidate, else candidate[0]. So a key the
        // CLI writes is always a key `web_search` reads back.
        let candidates = oz_tools::web_search::search_config_candidates("/tmp/wd");
        let path = search_config_write_path("/tmp/wd");
        assert_eq!(candidates[0], path);
        assert!(
            path.ends_with("mykey.toml"),
            "unexpected write path: {}",
            path.display()
        );
    }
}
