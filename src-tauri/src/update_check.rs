//! Lightweight update notification for builds without the signed Tauri
//! updater.
//!
//! The full updater needs a signing key and a `latest.json` manifest. Until
//! those exist, the app can still tell the user that a newer GitHub Release is
//! available and open its download page. This is deliberately read-only: it
//! never downloads or replaces the running bundle.

use std::time::Duration;

use serde::Serialize;

const LATEST_RELEASE_API: &str = "https://api.github.com/repos/V1sMu21/OpenZen/releases/latest";
const RELEASE_PAGE_PREFIX: &str = "https://github.com/V1sMu21/OpenZen/releases/";

#[derive(Debug, Serialize)]
pub struct LatestRelease {
    pub tag_name: String,
    pub html_url: String,
    pub body: String,
    pub published_at: String,
}

/// Fetch the latest published (non-draft, non-prerelease) GitHub Release.
///
/// The frontend compares `tag_name` against the running app version. Network
/// and rate-limit errors are returned so the caller can stay silent instead of
/// showing a broken update card.
#[tauri::command]
pub async fn check_latest_release() -> Result<LatestRelease, String> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("OpenZen/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .get(LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        return Err(format!(
            "GitHub release check failed: HTTP {}",
            response.status()
        ));
    }

    let value: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    let tag_name = value
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "GitHub release response has no tag_name".to_string())?
        .to_string();
    let html_url = value
        .get("html_url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "GitHub release response has no html_url".to_string())?
        .to_string();

    Ok(LatestRelease {
        tag_name,
        html_url,
        body: value
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        published_at: value
            .get("published_at")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

fn is_allowed_release_url(url: &str) -> bool {
    url.starts_with(RELEASE_PAGE_PREFIX)
        && !url.chars().any(|c| c.is_whitespace())
        && !url.contains('\0')
}

/// Open a GitHub Release page in the user's browser.
///
/// Only URLs under this repository's `/releases/` path are accepted: the
/// command must not become a general-purpose "open any URL" primitive.
#[tauri::command]
pub fn open_release_page(url: String) -> Result<(), String> {
    if !is_allowed_release_url(&url) {
        return Err("Refusing to open a non-release URL".into());
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&url)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("Opening release pages is only implemented on macOS".into())
    }
}

#[cfg(test)]
mod tests {
    use super::is_allowed_release_url;

    #[test]
    fn only_open_this_repositorys_release_pages() {
        assert!(is_allowed_release_url(
            "https://github.com/V1sMu21/OpenZen/releases/tag/v0.3.0"
        ));
        assert!(is_allowed_release_url(
            "https://github.com/V1sMu21/OpenZen/releases/latest"
        ));
        assert!(!is_allowed_release_url("https://github.com/other/repo"));
        assert!(!is_allowed_release_url(
            "https://github.com/V1sMu21/OpenZen/issues"
        ));
        assert!(!is_allowed_release_url(
            "https://github.com/V1sMu21/OpenZen/releases/tag/v0.3.0 extra"
        ));
    }
}
