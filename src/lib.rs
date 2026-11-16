//! arch-update-notifier: background update checker for Arch Linux.
//!
//! The pure parts (parsers, version comparison, state and summary rendering,
//! notification policy) are kept free of I/O so they can be unit-tested with
//! fixtures. The runtime parts (subprocesses, D-Bus, GSettings) live in their
//! own modules and are wired together in `daemon`.

pub mod checkers;
pub mod config;
pub mod daemon;
pub mod dbus;
pub mod exec;
pub mod glob;
pub mod logging;
pub mod network;
pub mod notify;
pub mod parse;
pub mod settings;
pub mod state;
pub mod vercmp;

/// Extension UUID; the daemon looks for the GSettings schema in this
/// extension's `schemas/` directory.
pub const EXTENSION_UUID: &str = "arch-update-notifier@nodefive.github.io";

/// GSettings schema id shared by the extension, prefs and daemon.
pub const SCHEMA_ID: &str = "org.gnome.shell.extensions.arch-update-notifier";

/// Unix time in seconds.
pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
