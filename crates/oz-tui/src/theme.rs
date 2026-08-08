//! OpenZen TUI color theme.
//!
//! The default palette is the Song Dynasty (宋韵) system.
//! Users can override colours via `[tui.theme]` in `mykey.toml`:
//!
//! ```toml
//! [tui.theme]
//! user_fg = "#6B9BB5"
//! agent_fg = "#8FBCBB"
//! ```
//!
//! A built-in `light` theme is also available via `/theme light`.

use ratatui::style::Color;
use serde::Deserialize;

// ── Built-in light theme ──

const THEME_LIGHT: Theme = Theme {
    user_fg: Some(Color::Rgb(50, 90, 140)),
    agent_fg: Some(Color::Rgb(30, 30, 30)),
    muted_fg: Some(Color::Rgb(130, 130, 130)),
    accent_fg: Some(Color::Rgb(40, 80, 130)),
    highlight_fg: Some(Color::Rgb(180, 120, 30)),
    tool_running: Some(Color::Rgb(180, 160, 0)),
    tool_done: Some(Color::Rgb(0, 140, 60)),
    tool_error: Some(Color::Rgb(180, 30, 30)),
    thinking_fg: Some(Color::Rgb(140, 60, 140)),
};

// ── Default Song Dynasty (dark) ──

const THEME_DARK: Theme = Theme {
    user_fg: Some(Color::Rgb(107, 155, 181)),
    agent_fg: Some(Color::Rgb(143, 188, 187)),
    muted_fg: Some(Color::Rgb(184, 197, 197)),
    accent_fg: Some(Color::Rgb(74, 107, 138)),
    highlight_fg: Some(Color::Rgb(212, 168, 89)),
    tool_running: Some(Color::Yellow),
    tool_done: Some(Color::Green),
    tool_error: Some(Color::Red),
    thinking_fg: Some(Color::Magenta),
};

#[derive(Debug, Clone, Deserialize)]
pub struct ThemeConfig {
    pub user_fg: Option<String>,
    pub agent_fg: Option<String>,
    pub muted_fg: Option<String>,
    pub accent_fg: Option<String>,
    pub highlight_fg: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Theme {
    user_fg: Option<Color>,
    agent_fg: Option<Color>,
    muted_fg: Option<Color>,
    accent_fg: Option<Color>,
    highlight_fg: Option<Color>,
    tool_running: Option<Color>,
    tool_done: Option<Color>,
    tool_error: Option<Color>,
    thinking_fg: Option<Color>,
}

impl Theme {
    pub const fn user_fg(&self) -> Color {
        unwrap_color(self.user_fg, Color::Rgb(107, 155, 181))
    }
    pub const fn agent_fg(&self) -> Color {
        unwrap_color(self.agent_fg, Color::Rgb(143, 188, 187))
    }
    pub const fn muted_fg(&self) -> Color {
        unwrap_color(self.muted_fg, Color::Rgb(184, 197, 197))
    }
    pub const fn accent_fg(&self) -> Color {
        unwrap_color(self.accent_fg, Color::Rgb(74, 107, 138))
    }
    pub const fn highlight_fg(&self) -> Color {
        unwrap_color(self.highlight_fg, Color::Rgb(212, 168, 89))
    }
    pub const fn tool_running(&self) -> Color {
        unwrap_color(self.tool_running, Color::Yellow)
    }
    pub const fn tool_done(&self) -> Color {
        unwrap_color(self.tool_done, Color::Green)
    }
    pub const fn tool_error(&self) -> Color {
        unwrap_color(self.tool_error, Color::Red)
    }
    pub const fn thinking_fg(&self) -> Color {
        unwrap_color(self.thinking_fg, Color::Magenta)
    }

    pub fn from_config(cfg: &ThemeConfig) -> Self {
        let mut t = THEME_DARK.clone();
        if let Some(ref s) = cfg.user_fg {
            if let Some(c) = parse_hex(s) { t.user_fg = Some(c); }
        }
        if let Some(ref s) = cfg.agent_fg {
            if let Some(c) = parse_hex(s) { t.agent_fg = Some(c); }
        }
        if let Some(ref s) = cfg.muted_fg {
            if let Some(c) = parse_hex(s) { t.muted_fg = Some(c); }
        }
        if let Some(ref s) = cfg.accent_fg {
            if let Some(c) = parse_hex(s) { t.accent_fg = Some(c); }
        }
        if let Some(ref s) = cfg.highlight_fg {
            if let Some(c) = parse_hex(s) { t.highlight_fg = Some(c); }
        }
        t
    }

    pub fn light() -> Self { THEME_LIGHT.clone() }
    pub fn dark() -> Self { THEME_DARK.clone() }
}

impl Default for Theme {
    fn default() -> Self { THEME_DARK.clone() }
}

const fn unwrap_color(opt: Option<Color>, default: Color) -> Color {
    match opt {
        Some(c) => c,
        None => default,
    }
}

fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim();
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() != 6 { return None; }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

// ── Static defaults (for modules that don't have access to App) ──

pub const USER_FG: Color = Color::Rgb(107, 155, 181);
pub const AGENT_FG: Color = Color::Rgb(143, 188, 187);
pub const MUTED_FG: Color = Color::Rgb(184, 197, 197);
pub const ACCENT_FG: Color = Color::Rgb(74, 107, 138);
pub const HIGHLIGHT_FG: Color = Color::Rgb(212, 168, 89);

pub const TOOL_RUNNING: Color = Color::Yellow;
pub const TOOL_DONE: Color = Color::Green;
pub const TOOL_ERROR: Color = Color::Red;
pub const THINKING_FG: Color = Color::Magenta;

pub const LOADING_FRAMES: &[&str] = &["░", "▒", "▓", "█", "▓", "▒"];
pub const IDLE_DOTS: &str = "○";

pub fn tool_emoji(name: &str) -> &'static str {
    match name {
        "read_file" | "glob" | "grep" | "search" | "ast_grep_search" | "ast_grep_replace" => "📖",
        "write" | "edit" | "patch" | "append" => "✏️",
        "bash" | "command" | "execute" | "run" | "code_run" => "💻",
        "web_search" | "webfetch" | "web" | "web_fetch" | "web_search_exa" => "🌐",
        "think" | "thinking" => "💭",
        "ask_user" => "❓",
        "rename" | "move" | "delete_file" | "create_directory" => "📁",
        "browser" | "screenshot" | "puppeteer" | "playwright" => "🖥️",
        "mcp" | "mcp_call" | "mcp_invoke" | "skill_mcp" => "🔌",
        _ => "⚙️",
    }
}

// ASCII FIGlet-style "OpenZen" logo (5 lines) plus tagline.
pub const OA_LOGO: &[&str] = &[
    "  ___                 ____         ",
    " / _ \\ _ __  ___ _ _ |_  /___ _ _  ",
    "| (_) | '_ \\/ -_) ' \\ / // -_) ' \\ ",
    " \\___/| .__/\\___|_||_/___\\___|_||_|",
    "      |_|                          ",
];

pub const TAGLINE: &str = "Work Less, Imagine More";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_is_dark() {
        let t = Theme::default();
        assert_eq!(t.user_fg(), Color::Rgb(107, 155, 181));
    }

    #[test]
    fn light_theme_is_different() {
        let t = Theme::light();
        assert_eq!(t.user_fg(), Color::Rgb(50, 90, 140));
    }

    #[test]
    fn parse_hex_valid() {
        assert_eq!(parse_hex("#FF0044"), Some(Color::Rgb(255, 0, 68)));
        assert_eq!(parse_hex("00aa33"), Some(Color::Rgb(0, 170, 51)));
    }

    #[test]
    fn parse_hex_invalid() {
        assert_eq!(parse_hex(""), None);
        assert_eq!(parse_hex("#xyz123"), None);
        assert_eq!(parse_hex("#12345"), None);
    }
}
