//! tracing setup: journald when running under systemd, stderr otherwise.
//! The level comes from GSettings and is changed at runtime through a reload
//! handle. Our crate gets the configured level; dependencies (zbus, hyper)
//! stay at WARN so `debug` does not drown the journal in D-Bus traffic.

use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::prelude::*;
use tracing_subscriber::{reload, Registry};

#[derive(Clone)]
pub struct LogHandle(reload::Handle<Targets, Registry>);

pub fn parse_level(s: &str) -> LevelFilter {
    match s.trim().to_ascii_lowercase().as_str() {
        "error" => LevelFilter::ERROR,
        "warn" | "warning" => LevelFilter::WARN,
        "debug" => LevelFilter::DEBUG,
        "trace" => LevelFilter::TRACE,
        _ => LevelFilter::INFO,
    }
}

fn targets(level: LevelFilter) -> Targets {
    Targets::new()
        .with_target("aun", level)
        .with_target("arch_update_notifier", level)
        // LevelFilter orders by verbosity, so min() picks the quieter one.
        .with_default(level.min(LevelFilter::WARN))
}

/// `force_stderr` is set by `--stderr` and `--once`.
pub fn init(force_stderr: bool) -> LogHandle {
    let (filter, handle) = reload::Layer::new(targets(LevelFilter::INFO));
    let use_journal = !force_stderr && std::env::var_os("JOURNAL_STREAM").is_some();
    let journald = if use_journal {
        tracing_journald::layer()
            .map(|l| l.with_syslog_identifier("arch-update-notifier".into()))
            .ok()
    } else {
        None
    };
    let fmt = journald.is_none().then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_target(false)
            .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
    });
    // try_init: never panic if a subscriber is somehow already set.
    let _ = tracing_subscriber::registry().with(filter).with(journald).with(fmt).try_init();
    LogHandle(handle)
}

impl LogHandle {
    pub fn set_level(&self, level: &str) {
        let lf = parse_level(level);
        if let Err(e) = self.0.reload(targets(lf)) {
            tracing::warn!(error = %e, "cannot change log level");
        } else {
            tracing::debug!(level = %lf, "log level set");
        }
    }
}
