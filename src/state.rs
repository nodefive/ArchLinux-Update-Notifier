//! state.json model, summary.txt rendering and notification policy.
//!
//! state.json is the contract with the GNOME extension (read via
//! Gio.FileMonitor) and the jq fallback of the shell snippet. Each source
//! stores both counted and ignored packages, so a change to the ignore list
//! can be applied immediately without running the checks again.

use crate::config::{Config, Source, TerminalConfig};
use crate::glob::is_ignored;
use crate::parse::Update;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// No check has completed yet.
    #[default]
    Unknown,
    Ok,
    Checking,
    /// At least one enabled source failed; its previous results are kept.
    Error,
    /// Check skipped (metered connection, offline).
    Skipped,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Unknown => "unknown",
            Status::Ok => "ok",
            Status::Checking => "checking",
            Status::Error => "error",
            Status::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceState {
    pub enabled: bool,
    pub error: Option<String>,
    pub checked_at: i64,
    /// Updates counted in the total.
    pub packages: Vec<Update>,
    /// Updates matched by the ignore list (not counted).
    pub ignored: Vec<Update>,
}

/// Memory for the notification policy, persisted so a daemon restart does
/// not repeat a notification.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyMemo {
    /// Total count after the previous completed check.
    pub prev_count: u32,
    /// `source/name/new` keys already announced.
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub version: u32,
    pub daemon_version: String,
    pub status: Status,
    pub message: Option<String>,
    /// Unix time of the last completed check (0 = never).
    pub last_check: i64,
    /// Unix time of the last check in which every enabled source succeeded.
    pub last_success: i64,
    pub next_check: i64,
    pub count: u32,
    pub counts: BTreeMap<Source, u32>,
    pub ignored_count: u32,
    pub sources: BTreeMap<Source, SourceState>,
    pub notify: NotifyMemo,
}

impl State {
    /// Load a previous state. A missing or malformed file yields an empty
    /// state; the next write replaces it.
    pub fn load(path: &Path) -> State {
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<State>(&bytes) {
                Ok(mut s) => {
                    // A crash mid-check would leave "checking" behind.
                    if s.status == Status::Checking {
                        s.status = Status::Unknown;
                    }
                    s
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "malformed state file, starting fresh");
                    State::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read state file, starting fresh");
                State::default()
            }
        }
    }

    /// Record the outcome of one source. On error the previous package list
    /// is kept (stale but still useful) and the error is attached.
    pub fn apply_result(&mut self, source: Source, result: Result<Vec<Update>, String>, now: i64, cfg: &Config) {
        let entry = self.sources.entry(source).or_default();
        entry.enabled = true;
        match result {
            Ok(all) => {
                let (ign, pk): (Vec<Update>, Vec<Update>) =
                    all.into_iter().partition(|u| is_ignored(&cfg.ignore, &u.name));
                entry.packages = pk;
                entry.ignored = ign;
                entry.error = None;
                entry.checked_at = now;
            }
            Err(e) => entry.error = Some(e),
        }
    }

    /// Re-apply the ignore list and enabled flags, then recount. Called after
    /// every check and whenever the settings change.
    pub fn refilter(&mut self, cfg: &Config) {
        for s in Source::ALL {
            let enabled = cfg.source(s).enabled;
            if !enabled {
                self.sources.remove(&s);
                continue;
            }
            let entry = self.sources.entry(s).or_default();
            entry.enabled = true;
            let mut all = std::mem::take(&mut entry.packages);
            all.append(&mut entry.ignored);
            let (ign, mut pk): (Vec<Update>, Vec<Update>) =
                all.into_iter().partition(|u| is_ignored(&cfg.ignore, &u.name));
            pk.sort();
            entry.packages = pk;
            entry.ignored = ign;
        }
        self.recount();
    }

    fn recount(&mut self) {
        self.counts = Source::ALL.iter().map(|s| (*s, 0)).collect();
        self.count = 0;
        self.ignored_count = 0;
        for (s, st) in &self.sources {
            let n = u32::try_from(st.packages.len()).unwrap_or(u32::MAX);
            self.counts.insert(*s, n);
            self.count = self.count.saturating_add(n);
            self.ignored_count = self
                .ignored_count
                .saturating_add(u32::try_from(st.ignored.len()).unwrap_or(u32::MAX));
        }
    }

    pub fn errors(&self) -> Vec<(Source, &str)> {
        self.sources
            .iter()
            .filter_map(|(s, st)| st.error.as_deref().map(|e| (*s, e)))
            .collect()
    }

    /// Keys identifying the counted updates, for the notification policy.
    pub fn update_keys(&self) -> BTreeSet<String> {
        self.sources
            .iter()
            .flat_map(|(s, st)| st.packages.iter().map(move |u| format!("{}/{}/{}", s.key(), u.name, u.new)))
            .collect()
    }

    /// Write state.json atomically (temp file + rename) so readers never see
    /// a partial file.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        write_atomic(path, &bytes)
    }

    /// Render summary.txt for the shell snippet. Line 1 is a tab-separated
    /// header: `AUN1 enabled last_check last_success stale_seconds`. The
    /// remaining lines are printed verbatim by the shell, with `{ago}`
    /// replaced by the relative age of `last_check` (computed at print time,
    /// since the file may be hours old).
    pub fn render_summary(&self, t: &TerminalConfig) -> String {
        let mut s = String::new();
        let stale = u64::from(t.stale_hours) * 3600;
        let _ = writeln!(
            s,
            "AUN1\t{}\t{}\t{}\t{}",
            u8::from(t.enabled),
            self.last_check,
            self.last_success,
            stale
        );
        if !t.enabled {
            return s;
        }

        let errors = self.errors();
        if self.count == 0 {
            if t.hide_zero {
                return s;
            }
            if self.last_check == 0 {
                s.push_str("No update check has completed yet\n");
            } else {
                s.push_str("System is up to date — last checked {ago}\n");
            }
        } else {
            let noun = if self.count == 1 { "update" } else { "updates" };
            let _ = write!(s, "{} {} available", self.count, noun);
            if t.show_breakdown {
                let parts: Vec<String> = Source::ALL
                    .iter()
                    .filter_map(|src| {
                        let n = self.counts.get(src).copied().unwrap_or(0);
                        (n > 0).then(|| format!("{} {}", n, src.label()))
                    })
                    .collect();
                if !parts.is_empty() {
                    let _ = write!(s, " ({})", parts.join(", "));
                }
            }
            s.push_str(" — last checked {ago}\n");

            if t.list_max > 0 && self.count <= t.list_max {
                let rows: Vec<(String, &Update)> = self
                    .sources
                    .iter()
                    .flat_map(|(src, st)| {
                        st.packages.iter().map(move |u| {
                            let name = match src {
                                Source::Repo => u.name.clone(),
                                other => format!("{} [{}]", u.name, other.label()),
                            };
                            (name, u)
                        })
                    })
                    .collect();
                let width = rows.iter().map(|(n, _)| n.chars().count()).max().unwrap_or(0);
                for (name, u) in rows {
                    let _ = writeln!(s, "  {name:<width$}  {} → {}", u.old, u.new);
                }
            }
        }
        for (src, err) in errors {
            let _ = writeln!(s, "  ! {} check failed: {}", src.label(), err);
        }
        s
    }
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "state".into());
    let tmp = dir.join(format!(".{file_name}.tmp"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Decide whether to notify, and return the updated memo.
///
/// Rules: notify only when the set contains something not announced before
/// (so upgrading part of the list does not re-notify), the total reaches
/// `notify_min_count`, and, with `notify_only_increase`, the total grew
/// since the previous check. Suppressed new items stay "unannounced" so they
/// can still trigger a notification once the thresholds are met.
pub fn decide_notification(cfg: &Config, keys: &BTreeSet<String>, count: u32, memo: &NotifyMemo) -> (bool, NotifyMemo) {
    let announced: BTreeSet<&str> = memo.keys.iter().map(String::as_str).collect();
    let has_new = keys.iter().any(|k| !announced.contains(k.as_str()));

    let notify = cfg.notify_enabled
        && count > 0
        && count >= cfg.notify_min_count
        && has_new
        && (!cfg.notify_only_increase || count > memo.prev_count);

    let keys_out: Vec<String> = if notify || !has_new {
        // Announced now, or nothing new: remember the current set (this also
        // forgets packages that were upgraded away).
        keys.iter().cloned().collect()
    } else {
        // Keep only still-present announced keys; new ones stay pending.
        memo.keys.iter().filter(|k| keys.contains(*k)).cloned().collect()
    };
    (
        notify,
        NotifyMemo {
            prev_count: count,
            keys: keys_out,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::default()
    }

    fn state_with(repo: &[(&str, &str, &str)], aur: &[(&str, &str, &str)], cfg: &Config) -> State {
        let mut s = State::default();
        let mk = |v: &[(&str, &str, &str)]| v.iter().map(|(n, o, w)| Update::new(*n, *o, *w)).collect::<Vec<_>>();
        s.apply_result(Source::Repo, Ok(mk(repo)), 100, cfg);
        s.apply_result(Source::Aur, Ok(mk(aur)), 100, cfg);
        s.last_check = 100;
        s.last_success = 100;
        s.refilter(cfg);
        s
    }

    #[test]
    fn ignore_list_applies_and_reapplies() {
        let mut c = cfg();
        c.ignore = vec!["linux*".into()];
        let mut s = state_with(&[("linux", "1", "2"), ("mesa", "1", "2")], &[("yay", "1", "2")], &c);
        assert_eq!(s.count, 2);
        assert_eq!(s.ignored_count, 1);
        c.ignore.clear();
        s.refilter(&c);
        assert_eq!(s.count, 3);
        assert_eq!(s.ignored_count, 0);
    }

    #[test]
    fn failed_source_keeps_previous_packages() {
        let c = cfg();
        let mut s = state_with(&[("mesa", "1", "2")], &[("yay", "1", "2")], &c);
        s.apply_result(Source::Aur, Err("timeout".into()), 200, &c);
        s.refilter(&c);
        assert_eq!(s.count, 2);
        assert_eq!(s.errors(), vec![(Source::Aur, "timeout")]);
    }

    #[test]
    fn disabling_a_source_drops_it() {
        let mut c = cfg();
        let mut s = state_with(&[("mesa", "1", "2")], &[("yay", "1", "2")], &c);
        c.source_mut(Source::Aur).enabled = false;
        s.refilter(&c);
        assert_eq!(s.count, 1);
        assert!(!s.sources.contains_key(&Source::Aur));
    }

    #[test]
    fn summary_breakdown_and_list() {
        let c = cfg();
        let s = state_with(&[("mesa", "1", "2"), ("linux", "6.1", "6.2")], &[("yay", "12", "13")], &c);
        let txt = s.render_summary(&c.terminal);
        let lines: Vec<&str> = txt.lines().collect();
        assert_eq!(lines[0], "AUN1\t1\t100\t100\t86400");
        assert_eq!(lines[1], "3 updates available (2 repo, 1 AUR) — last checked {ago}");
        assert_eq!(lines[2], "  linux      6.1 → 6.2");
        assert_eq!(lines[3], "  mesa       1 → 2");
        assert_eq!(lines[4], "  yay [AUR]  12 → 13");
    }

    #[test]
    fn summary_hides_zero_and_respects_disable() {
        let mut c = cfg();
        let s = state_with(&[], &[], &c);
        assert_eq!(s.render_summary(&c.terminal).lines().count(), 1);
        c.terminal.hide_zero = false;
        assert!(s.render_summary(&c.terminal).contains("up to date"));
        c.terminal.enabled = false;
        assert!(s.render_summary(&c.terminal).starts_with("AUN1\t0\t"));
    }

    #[test]
    fn notification_policy() {
        let mut c = cfg();
        let keys = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        let memo = NotifyMemo::default();

        let (n, memo) = decide_notification(&c, &keys(&["repo/a/2", "repo/b/2"]), 2, &memo);
        assert!(n, "first updates notify");
        let (n, memo) = decide_notification(&c, &keys(&["repo/a/2"]), 1, &memo);
        assert!(!n, "partial upgrade does not re-notify");
        let (n, memo) = decide_notification(&c, &keys(&["repo/a/2", "repo/c/1"]), 2, &memo);
        assert!(n, "a new package notifies");

        c.notify_only_increase = true;
        let (n, memo) = decide_notification(&c, &keys(&["repo/d/1"]), 1, &memo);
        assert!(!n, "new package but count dropped");
        let (n, _) = decide_notification(&c, &keys(&["repo/d/1", "repo/e/1"]), 2, &memo);
        assert!(n, "pending new item announced once count grows");

        c.notify_only_increase = false;
        c.notify_min_count = 5;
        let (n, _) = decide_notification(&c, &keys(&["repo/x/1"]), 1, &NotifyMemo::default());
        assert!(!n, "below minimum");
    }
}
