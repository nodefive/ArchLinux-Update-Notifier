//! Runtime configuration, a plain-data snapshot of the GSettings keys.
//!
//! `Config` is `Send` and cheap to clone, so the GSettings thread can hand
//! complete snapshots to the tokio side instead of sharing GObjects.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Update source. Serialized in lowercase (`repo`, `aur`, ...) in state.json.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Repo,
    Aur,
    Flatpak,
    Snap,
}

impl Source {
    pub const ALL: [Source; 4] = [Source::Repo, Source::Aur, Source::Flatpak, Source::Snap];

    pub fn key(self) -> &'static str {
        match self {
            Source::Repo => "repo",
            Source::Aur => "aur",
            Source::Flatpak => "flatpak",
            Source::Snap => "snap",
        }
    }

    /// Short label used in the terminal breakdown ("18 repo, 4 AUR").
    pub fn label(self) -> &'static str {
        match self {
            Source::Repo => "repo",
            Source::Aur => "AUR",
            Source::Flatpak => "flatpak",
            Source::Snap => "snap",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AurHelper {
    Auto,
    Paru,
    Yay,
    Rpc,
}

impl AurHelper {
    pub fn from_key(s: &str) -> Self {
        match s {
            "paru" => AurHelper::Paru,
            "yay" => AurHelper::Yay,
            "rpc" => AurHelper::Rpc,
            _ => AurHelper::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceConfig {
    pub enabled: bool,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalConfig {
    pub enabled: bool,
    pub hide_zero: bool,
    pub show_breakdown: bool,
    /// List package names when the total is at most this many (0 = never).
    pub list_max: u32,
    /// Warn when the last successful check is older than this (0 = never).
    pub stale_hours: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub interval: Duration,
    pub initial_delay: Duration,
    pub sources: [SourceConfig; 4],
    pub aur_helper: AurHelper,
    pub unmetered_only: bool,
    pub notify_enabled: bool,
    pub notify_min_count: u32,
    pub notify_only_increase: bool,
    pub terminal: TerminalConfig,
    pub log_level: String,
    pub ignore: Vec<String>,
}

impl Config {
    pub fn source(&self, s: Source) -> SourceConfig {
        self.sources[s as usize]
    }

    pub fn source_mut(&mut self, s: Source) -> &mut SourceConfig {
        &mut self.sources[s as usize]
    }

    pub fn enabled_sources(&self) -> impl Iterator<Item = Source> + '_ {
        Source::ALL.into_iter().filter(|s| self.source(*s).enabled)
    }
}

/// Defaults mirror the gschema so the daemon behaves identically when the
/// schema cannot be found.
impl Default for Config {
    fn default() -> Self {
        let src = |enabled, secs| SourceConfig {
            enabled,
            timeout: Duration::from_secs(secs),
        };
        Config {
            interval: Duration::from_secs(60 * 60),
            initial_delay: Duration::from_secs(60),
            sources: [src(true, 180), src(true, 120), src(true, 120), src(false, 60)],
            aur_helper: AurHelper::Auto,
            unmetered_only: false,
            notify_enabled: true,
            notify_min_count: 1,
            notify_only_increase: false,
            terminal: TerminalConfig {
                enabled: true,
                hide_zero: true,
                show_breakdown: true,
                list_max: 5,
                stale_hours: 24,
            },
            log_level: "info".into(),
            ignore: Vec::new(),
        }
    }
}
