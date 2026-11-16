//! Subprocess execution with timeouts.
//!
//! Every command runs with an argv array (never through a shell), `LC_ALL=C`
//! for stable output, stdin closed, and in its own process group so that a
//! timeout or daemon shutdown also kills grandchildren (checkupdates spawns
//! fakeroot/pacman, paru/yay may spawn git).

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("{0} is not installed")]
    NotFound(String),
    #[error("{prog} timed out after {secs}s")]
    Timeout { prog: String, secs: u64 },
    #[error("failed to run {prog}: {source}")]
    Io {
        prog: String,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// Last meaningful stderr line, for short error messages.
    pub fn stderr_summary(&self) -> String {
        let line = self
            .stderr
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or("no error output");
        let line = crate::parse::strip_ansi(line);
        let mut s: String = line.chars().take(200).collect();
        if s.is_empty() {
            s.push_str("unknown error");
        }
        s
    }

    pub fn code_str(&self) -> String {
        self.code.map_or_else(|| "signal".to_string(), |c| c.to_string())
    }
}

/// Kills the child's whole process group on drop unless disarmed. Dropping
/// happens on timeout and when the check task is aborted at shutdown.
struct GroupGuard(Option<u32>);

impl GroupGuard {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            if let Ok(pgid) = libc::pid_t::try_from(pid) {
                // SAFETY: kill(2) has no memory-safety preconditions. The
                // group id is our direct child's pid (process_group(0)), and
                // the child has not been reaped yet, so the id cannot have
                // been reused.
                unsafe {
                    libc::kill(-pgid, libc::SIGKILL);
                }
            }
        }
    }
}

pub async fn run(
    prog: &str,
    args: &[&str],
    env: &[(&str, &OsStr)],
    timeout: Duration,
) -> Result<Output, ExecError> {
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    for (k, v) in env {
        cmd.env(k, v);
    }

    let child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ExecError::NotFound(prog.to_string())
        } else {
            ExecError::Io {
                prog: prog.to_string(),
                source: e,
            }
        }
    })?;
    let pid = child.id();
    let fut = child.wait_with_output();
    tokio::pin!(fut);
    // Declared after `fut`, so it is dropped first: on timeout or abort the
    // group is killed while the direct child is still unreaped.
    let mut guard = GroupGuard(pid);

    match tokio::time::timeout(timeout, &mut fut).await {
        Ok(Ok(out)) => {
            guard.disarm();
            Ok(Output {
                code: out.status.code(),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            })
        }
        Ok(Err(e)) => Err(ExecError::Io {
            prog: prog.to_string(),
            source: e,
        }),
        Err(_) => Err(ExecError::Timeout {
            prog: prog.to_string(),
            secs: timeout.as_secs(),
        }),
    }
}

/// Look a program up in $PATH without spawning anything.
pub fn find_in_path(prog: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(prog))
        .find(|p| {
            std::fs::metadata(p)
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}
