//! The per-source checks. Each returns the full (unfiltered) update list or a
//! short error string for the UI. Nothing here needs root or touches the
//! system package databases: checkupdates syncs a private copy of the sync
//! databases, and the AUR checks only query.

use crate::config::{AurHelper, Config, Source};
use crate::exec::{self, ExecError};
use crate::network::{self, NetState};
use crate::parse::{self, Update};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub type SourceOutcome = Result<Vec<Update>, String>;

#[derive(Debug)]
pub enum CheckOutcome {
    /// Check not performed; retry after the given delay.
    Skipped { reason: String, retry_in: Duration },
    Done(Vec<(Source, SourceOutcome)>),
}

/// libalpm lock file. Its presence means a pacman transaction is running;
/// copying the local DB at that moment could produce inconsistent results.
const PACMAN_LOCK: &str = "/var/lib/pacman/db.lck";
pub const PACMAN_LOCAL_DB: &str = "/var/lib/pacman/local";

const AUR_RPC: &str = "https://aur.archlinux.org/rpc/v5/info";
/// Names per RPC request; keeps the URL well below server limits.
const AUR_CHUNK: usize = 150;

pub struct Checker {
    http: reqwest::Client,
    /// Private CHECKUPDATES_DB, so the daemon never collides with a manual
    /// `checkupdates` run on the default /tmp path.
    checkup_db: PathBuf,
}

impl Checker {
    pub fn new(cache_dir: &Path) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("arch-update-notifier/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .build()?;
        Ok(Checker {
            http,
            checkup_db: cache_dir.join("checkup-db"),
        })
    }

    /// Run all enabled sources concurrently. `manual` checks ignore the
    /// "unmetered only" setting (the user asked explicitly).
    pub async fn run_all(&self, cfg: &Config, manual: bool) -> CheckOutcome {
        match network::query().await {
            NetState::Offline => {
                return CheckOutcome::Skipped {
                    reason: "no network connection".into(),
                    retry_in: Duration::from_secs(5 * 60),
                }
            }
            NetState::Online { metered: true } if cfg.unmetered_only && !manual => {
                return CheckOutcome::Skipped {
                    reason: "metered connection".into(),
                    retry_in: cfg.interval.min(Duration::from_secs(15 * 60)),
                }
            }
            _ => {}
        }

        let run = |s: Source| async move {
            if cfg.source(s).enabled {
                Some((s, self.check(s, cfg).await))
            } else {
                None
            }
        };
        let (r, a, f, s) = tokio::join!(
            run(Source::Repo),
            run(Source::Aur),
            run(Source::Flatpak),
            run(Source::Snap)
        );
        CheckOutcome::Done([r, a, f, s].into_iter().flatten().collect())
    }

    pub async fn check(&self, source: Source, cfg: &Config) -> SourceOutcome {
        let timeout = cfg.source(source).timeout;
        let started = std::time::Instant::now();
        let res = match source {
            Source::Repo => self.repo(timeout).await,
            Source::Aur => self.aur(cfg.aur_helper, timeout).await,
            Source::Flatpak => self.flatpak(timeout).await,
            Source::Snap => self.snap(timeout).await,
        };
        match &res {
            Ok(v) => tracing::info!(source = source.key(), count = v.len(), elapsed_ms = started.elapsed().as_millis() as u64, "check done"),
            Err(e) => tracing::warn!(source = source.key(), error = %e, "check failed"),
        }
        res
    }

    async fn repo(&self, timeout: Duration) -> SourceOutcome {
        // Wait (bounded) for a running pacman transaction to finish.
        let deadline = tokio::time::Instant::now() + timeout.min(Duration::from_secs(120));
        while Path::new(PACMAN_LOCK).exists() {
            if tokio::time::Instant::now() >= deadline {
                return Err("pacman database is locked (transaction in progress)".into());
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        let db = self.checkup_db.as_os_str();
        let out = exec::run("checkupdates", &["--nocolor"], &[("CHECKUPDATES_DB", db)], timeout)
            .await
            .map_err(|e| match e {
                ExecError::NotFound(_) => "checkupdates not found (install pacman-contrib)".to_string(),
                other => other.to_string(),
            })?;
        // checkupdates: 0 = updates listed, 2 = no updates, 1 = error.
        match out.code {
            Some(0) => Ok(parse::parse_pacman_updates(&out.stdout)),
            Some(2) => Ok(Vec::new()),
            _ => Err(format!("checkupdates failed ({}): {}", out.code_str(), out.stderr_summary())),
        }
    }

    async fn aur(&self, helper: AurHelper, timeout: Duration) -> SourceOutcome {
        let resolved = match helper {
            AurHelper::Rpc => None,
            AurHelper::Paru | AurHelper::Yay => {
                let name = if helper == AurHelper::Paru { "paru" } else { "yay" };
                if exec::find_in_path(name).is_some() {
                    Some(name)
                } else {
                    tracing::warn!(helper = name, "configured AUR helper not found, using AUR RPC");
                    None
                }
            }
            AurHelper::Auto => ["paru", "yay"].into_iter().find(|h| exec::find_in_path(h).is_some()),
        };
        match resolved {
            Some(h) => self.aur_helper(h, timeout).await,
            None => tokio::time::timeout(timeout, self.aur_rpc(timeout))
                .await
                .unwrap_or_else(|_| Err(format!("AUR RPC timed out after {}s", timeout.as_secs()))),
        }
    }

    async fn aur_helper(&self, helper: &str, timeout: Duration) -> SourceOutcome {
        let out = exec::run(helper, &["-Qua"], &[], timeout).await.map_err(|e| e.to_string())?;
        let updates = parse::parse_pacman_updates(&out.stdout);
        match out.code {
            Some(0) => Ok(updates),
            // Like pacman -Qu, helpers exit 1 when there is nothing to list.
            Some(1) if updates.is_empty() && !looks_like_error(&out.stderr) => Ok(Vec::new()),
            _ => Err(format!("{helper} -Qua failed ({}): {}", out.code_str(), out.stderr_summary())),
        }
    }

    async fn aur_rpc(&self, timeout: Duration) -> SourceOutcome {
        let out = exec::run("pacman", &["-Qm"], &[], timeout).await.map_err(|e| e.to_string())?;
        let local = parse::parse_foreign_packages(&out.stdout);
        // pacman -Qm exits 1 when there are no foreign packages.
        if local.is_empty() {
            return if out.code == Some(0) || out.code == Some(1) {
                Ok(Vec::new())
            } else {
                Err(format!("pacman -Qm failed: {}", out.stderr_summary()))
            };
        }

        let mut remote = Vec::with_capacity(local.len());
        for chunk in local.chunks(AUR_CHUNK) {
            let mut url = String::from(AUR_RPC);
            for (i, (name, _)) in chunk.iter().enumerate() {
                url.push(if i == 0 { '?' } else { '&' });
                url.push_str("arg%5B%5D=");
                url.push_str(&percent_encode(name));
            }
            let resp = self
                .http
                .get(&url)
                .timeout(timeout)
                .send()
                .await
                .map_err(|e| format!("AUR RPC request failed: {}", error_chain(&e)))?;
            let status = resp.status();
            let body = resp
                .text()
                .await
                .map_err(|e| format!("AUR RPC read failed: {}", error_chain(&e)))?;
            if !status.is_success() {
                return Err(format!("AUR RPC returned HTTP {status}"));
            }
            remote.extend(parse::parse_aur_rpc(&body).map_err(|e| e.to_string())?);
        }
        Ok(parse::aur_updates(&local, &remote))
    }

    async fn flatpak(&self, timeout: Duration) -> SourceOutcome {
        if exec::find_in_path("flatpak").is_none() {
            tracing::debug!("flatpak not installed, nothing to check");
            return Ok(Vec::new());
        }
        let list = exec::run("flatpak", &["list", "--columns=ref:f,version:f,active:f"], &[], timeout);
        let remote = exec::run(
            "flatpak",
            &["remote-ls", "--updates", "--columns=ref:f,version:f,commit:f"],
            &[],
            timeout,
        );
        let (list, remote) = tokio::join!(list, remote);
        let list = list.map_err(|e| e.to_string())?;
        let remote = remote.map_err(|e| e.to_string())?;
        if remote.code != Some(0) {
            return Err(format!("flatpak remote-ls failed: {}", remote.stderr_summary()));
        }
        let installed = parse::parse_flatpak_columns(&list.stdout);
        let updates = parse::parse_flatpak_columns(&remote.stdout);
        Ok(parse::flatpak_updates(&installed, &updates))
    }

    async fn snap(&self, timeout: Duration) -> SourceOutcome {
        if exec::find_in_path("snap").is_none() {
            tracing::debug!("snap not installed, nothing to check");
            return Ok(Vec::new());
        }
        let list = exec::run("snap", &["list"], &[], timeout);
        let refresh = exec::run("snap", &["refresh", "--list"], &[], timeout);
        let (list, refresh) = tokio::join!(list, refresh);
        let list = list.map_err(|e| e.to_string())?;
        let refresh = refresh.map_err(|e| e.to_string())?;
        if refresh.code != Some(0) {
            return Err(format!("snap refresh --list failed: {}", refresh.stderr_summary()));
        }
        // "All snaps up to date." goes to stderr with an empty stdout.
        let installed = parse::parse_snap_table(&list.stdout);
        let pending = parse::parse_snap_table(&refresh.stdout);
        Ok(parse::snap_updates(&installed, &pending))
    }
}

fn looks_like_error(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("error") || s.contains("failed")
}

/// Percent-encode everything outside RFC 3986 unreserved characters (AUR
/// names may contain '+', '@' and '.').
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// reqwest errors hide the useful part (DNS failure, TLS error) in sources.
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut msg = e.to_string();
    let mut cur = e.source();
    while let Some(s) = cur {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        cur = s.source();
    }
    msg
}

/// Modification time of the local package DB directory; changes whenever a
/// package is installed, upgraded or removed.
pub fn pacman_local_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(PACMAN_LOCAL_DB).and_then(|m| m.modified()).ok()
}

pub fn pacman_locked() -> bool {
    Path::new(PACMAN_LOCK).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_aur_names() {
        assert_eq!(percent_encode("gtk+3-git"), "gtk%2B3-git");
        assert_eq!(percent_encode("foo@bar"), "foo%40bar");
        assert_eq!(percent_encode("python-a.b_c"), "python-a.b_c");
    }

    #[test]
    fn helper_error_detection() {
        assert!(looks_like_error("error: failed to synchronize"));
        assert!(!looks_like_error(""));
    }

}
