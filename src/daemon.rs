//! Main loop. Single-threaded tokio runtime; one `select!` multiplexes:
//! the schedule timer, D-Bus commands, settings snapshots, the pacman DB
//! watcher, the running check task and SIGTERM/SIGINT.

use crate::checkers::{self, CheckOutcome, Checker};
use crate::config::Config;
use crate::dbus::{Bus, Command};
use crate::logging::LogHandle;
use crate::state::{self, State, Status};
use crate::{notify, now_epoch, settings};
use anyhow::Context as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinHandle};
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

#[derive(Debug, Default)]
pub struct Args {
    pub schema_dir: Option<PathBuf>,
    pub stderr: bool,
    pub once: bool,
}

pub struct Paths {
    pub dir: PathBuf,
    pub state: PathBuf,
    pub summary: PathBuf,
}

impl Paths {
    pub fn new() -> anyhow::Result<Paths> {
        // $XDG_CACHE_HOME/arch-update-notifier (glib honours XDG_CACHE_HOME).
        let dir = gio::glib::user_cache_dir().join("arch-update-notifier");
        std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
        Ok(Paths {
            state: dir.join("state.json"),
            summary: dir.join("summary.txt"),
            dir,
        })
    }
}

struct Daemon {
    cfg: Config,
    state: State,
    paths: Paths,
    bus: Option<Bus>,
    checker: Arc<Checker>,
    log: LogHandle,
    /// Status to restore if a check is aborted at shutdown.
    status_before_check: Status,
    notifications: bool,
}

/// What a settings change requires from the loop.
#[derive(Default)]
struct Reschedule {
    /// Run a check at this time (e.g. a source was enabled).
    at: Option<Instant>,
}

impl Daemon {
    fn persist(&mut self) {
        self.state.version = state::STATE_VERSION;
        self.state.daemon_version = env!("CARGO_PKG_VERSION").to_string();
        if let Err(e) = self.state.write(&self.paths.state) {
            error!(error = %e, path = %self.paths.state.display(), "cannot write state");
        }
        let summary = self.state.render_summary(&self.cfg.terminal);
        if let Err(e) = state::write_atomic(&self.paths.summary, summary.as_bytes()) {
            error!(error = %e, path = %self.paths.summary.display(), "cannot write summary");
        }
    }

    async fn publish(&self, updates_changed: bool) {
        if let Some(bus) = &self.bus {
            if let Err(e) = bus.publish(&self.state, updates_changed).await {
                warn!(error = %e, "D-Bus publish failed");
            }
        }
    }

    async fn start_check(&mut self, manual: bool) -> JoinHandle<CheckOutcome> {
        info!(manual, "starting check");
        if self.state.status != Status::Checking {
            self.status_before_check = self.state.status;
        }
        self.state.status = Status::Checking;
        self.state.message = None;
        self.persist();
        self.publish(false).await;

        let cfg = self.cfg.clone();
        let checker = Arc::clone(&self.checker);
        tokio::spawn(async move { checker.run_all(&cfg, manual).await })
    }

    /// Merge a finished check. Returns the delay until the next check.
    async fn finish_check(&mut self, res: Result<CheckOutcome, JoinError>) -> Duration {
        let now = now_epoch();
        let outcome = match res {
            Ok(o) => o,
            Err(e) => {
                error!(error = %e, "check task failed");
                self.state.status = Status::Error;
                self.state.message = Some(format!("internal error: {e}"));
                self.persist();
                self.publish(false).await;
                return Duration::from_secs(5 * 60);
            }
        };

        match outcome {
            CheckOutcome::Skipped { reason, retry_in } => {
                info!(%reason, retry_in_s = retry_in.as_secs(), "check skipped");
                self.state.status = Status::Skipped;
                self.state.message = Some(reason);
                self.persist();
                self.publish(false).await;
                retry_in
            }
            CheckOutcome::Done(results) => {
                let before = self.state.update_keys();
                for (src, r) in results {
                    self.state.apply_result(src, r, now, &self.cfg);
                }
                self.state.refilter(&self.cfg);
                let errors: Vec<String> =
                    self.state.errors().iter().map(|(s, e)| format!("{}: {e}", s.label())).collect();
                self.state.last_check = now;
                if errors.is_empty() {
                    self.state.status = Status::Ok;
                    self.state.message = None;
                    self.state.last_success = now;
                } else {
                    self.state.status = Status::Error;
                    self.state.message = Some(errors.join("; "));
                }
                info!(count = self.state.count, ignored = self.state.ignored_count, status = self.state.status.as_str(), "check finished");

                let keys = self.state.update_keys();
                let (should, memo) =
                    state::decide_notification(&self.cfg, &keys, self.state.count, &self.state.notify);
                self.state.notify = memo;
                self.persist();
                // Always signal after a check: LastCheck changed even if the
                // package set did not, and the extension shows the time.
                self.publish(true).await;
                debug!(changed = before != keys, "update set compared");

                if should && self.notifications {
                    if let Err(e) = notify::send(&self.state).await {
                        warn!(error = %e, "cannot send notification");
                    }
                }
                self.cfg.interval
            }
        }
    }

    async fn apply_config(&mut self, new: Config, running: bool) -> Reschedule {
        let mut out = Reschedule::default();
        if new == self.cfg {
            return out;
        }
        if new.log_level != self.cfg.log_level {
            self.log.set_level(&new.log_level);
        }
        let newly_enabled = crate::config::Source::ALL
            .iter()
            .any(|s| new.source(*s).enabled && !self.cfg.source(*s).enabled);
        let interval_changed = new.interval != self.cfg.interval;
        let view_changed = new.ignore != self.cfg.ignore
            || new.terminal != self.cfg.terminal
            || new.sources.iter().zip(self.cfg.sources.iter()).any(|(a, b)| a.enabled != b.enabled);
        self.cfg = new;
        info!("settings updated");

        if interval_changed && !running {
            let due = self.state.last_check + self.cfg.interval.as_secs() as i64;
            let wait = u64::try_from(due - now_epoch()).unwrap_or(0).max(10);
            out.at = Some(Instant::now() + Duration::from_secs(wait));
        }
        if newly_enabled {
            out.at = Some(Instant::now() + Duration::from_secs(5));
        }
        if view_changed {
            let count_before = self.state.count;
            self.state.refilter(&self.cfg);
            self.persist();
            self.publish(self.state.count != count_before).await;
        }
        out
    }

    /// Delay before the first check: honour the configured initial delay,
    /// but do not check again right away if the previous state is recent.
    fn initial_wait(&self) -> Duration {
        let delay = self.cfg.initial_delay;
        if self.state.last_check == 0 || self.state.status == Status::Unknown {
            return delay;
        }
        let due = self.state.last_check + self.cfg.interval.as_secs() as i64;
        let until_due = Duration::from_secs(u64::try_from(due - now_epoch()).unwrap_or(0));
        until_due.max(delay)
    }

    fn set_next(&mut self, next: Instant) {
        let secs = next.saturating_duration_since(Instant::now()).as_secs() as i64;
        self.state.next_check = now_epoch() + secs;
    }
}

async fn join_running(h: &mut Option<JoinHandle<CheckOutcome>>) -> Result<CheckOutcome, JoinError> {
    match h {
        Some(h) => h.await,
        None => std::future::pending().await,
    }
}

fn mtime_changed(prev: &mut Option<SystemTime>) -> bool {
    let cur = checkers::pacman_local_mtime();
    let changed = cur.is_some() && cur != *prev;
    *prev = cur;
    changed
}

pub async fn run(args: Args, log: LogHandle) -> anyhow::Result<()> {
    // reqwest is built with rustls-no-provider; pick ring explicitly. Err
    // means a provider is already installed, which is fine.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let paths = Paths::new()?;
    let (cfg_tx, mut cfg_rx) = mpsc::unbounded_channel::<Config>();
    let loaded = settings::spawn(args.schema_dir.clone(), cfg_tx)?;
    log.set_level(&loaded.initial.log_level);
    info!(version = env!("CARGO_PKG_VERSION"), schema = %loaded.origin, live = loaded.live, cache = %paths.dir.display(), "starting");

    let checker = Arc::new(Checker::new(&paths.dir)?);
    let mut state = State::load(&paths.state);
    state.refilter(&loaded.initial);

    let mut d = Daemon {
        cfg: loaded.initial,
        state,
        paths,
        bus: None,
        checker,
        log,
        status_before_check: Status::Unknown,
        notifications: !args.once,
    };

    if args.once {
        let h = d.start_check(true).await;
        d.finish_check(h.await).await;
        let summary = d.state.render_summary(&d.cfg.terminal);
        for line in summary.lines().skip(1) {
            println!("{}", line.replace("{ago}", "just now"));
        }
        if d.state.count == 0 && summary.lines().count() <= 1 {
            println!("{} updates (terminal message hidden by settings)", d.state.count);
        }
        return Ok(());
    }

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(1);
    d.bus = Some(Bus::start(cmd_tx, &d.state).await?);
    info!(name = crate::dbus::BUS_NAME, "D-Bus service ready");

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    let mut next = Instant::now() + d.initial_wait();
    d.set_next(next);
    d.persist();
    d.publish(false).await;

    let mut running: Option<JoinHandle<CheckOutcome>> = None;
    let mut queued_manual = false;
    let mut queued_auto = false;
    let mut db_tick = tokio::time::interval(Duration::from_secs(60));
    db_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut db_mtime = checkers::pacman_local_mtime();

    loop {
        let idle = running.is_none();
        tokio::select! {
            _ = sigterm.recv() => { info!("SIGTERM received"); break; }
            _ = sigint.recv() => { info!("SIGINT received"); break; }

            _ = tokio::time::sleep_until(next), if idle => {
                db_mtime = checkers::pacman_local_mtime();
                running = Some(d.start_check(false).await);
            }

            Some(cmd) = cmd_rx.recv() => match cmd {
                Command::CheckNow if idle => {
                    db_mtime = checkers::pacman_local_mtime();
                    running = Some(d.start_check(true).await);
                }
                Command::CheckNow => {
                    debug!("check requested while running, queued");
                    queued_manual = true;
                }
            },

            Some(cfg) = cfg_rx.recv() => {
                let r = d.apply_config(cfg, !idle).await;
                if let Some(at) = r.at {
                    if idle {
                        next = at;
                        d.set_next(next);
                    } else {
                        queued_auto = true;
                    }
                }
            }

            _ = db_tick.tick() => {
                // A package transaction finished (pacman, paru, the
                // "Upgrade now" action): re-check shortly after.
                if mtime_changed(&mut db_mtime) && !checkers::pacman_locked() {
                    info!("local package database changed, scheduling check");
                    if idle {
                        next = next.min(Instant::now() + Duration::from_secs(20));
                        d.set_next(next);
                    } else {
                        queued_auto = true;
                    }
                }
            }

            res = join_running(&mut running) => {
                running = None;
                let wait = d.finish_check(res).await;
                next = Instant::now() + wait;
                if queued_manual {
                    queued_manual = false;
                    queued_auto = false;
                    db_mtime = checkers::pacman_local_mtime();
                    running = Some(d.start_check(true).await);
                } else if queued_auto {
                    queued_auto = false;
                    next = Instant::now() + Duration::from_secs(20);
                }
                d.set_next(next);
                d.persist();
            }
        }
    }

    if let Some(h) = running.take() {
        // Aborting drops the task; exec's guards kill the process groups.
        h.abort();
        let _ = h.await;
        d.state.status = d.status_before_check;
    }
    d.persist();
    d.publish(false).await;
    info!("stopped");
    Ok(())
}
