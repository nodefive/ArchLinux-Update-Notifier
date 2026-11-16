//! Parsers for the output of checkupdates, paru/yay, pacman -Qm, the AUR RPC,
//! flatpak and snap. All functions are pure and tolerant: unknown lines
//! (warnings, headers, progress messages) are skipped, never fatal.

use crate::vercmp::vercmp;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Update {
    pub name: String,
    pub old: String,
    pub new: String,
}

impl Update {
    pub fn new(name: impl Into<String>, old: impl Into<String>, new: impl Into<String>) -> Self {
        Update {
            name: name.into(),
            old: old.into(),
            new: new.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("invalid AUR RPC response: {0}")]
    AurJson(#[from] serde_json::Error),
    #[error("AUR RPC error: {0}")]
    AurRpc(String),
}

/// Remove ANSI CSI escape sequences (colour codes). paru/yay may emit them
/// even when stdout is not a TTY, depending on the user's config.
pub fn strip_ansi(s: &str) -> Cow<'_, str> {
    if !s.contains('\x1b') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                // CSI: parameters/intermediates until a final byte in @..~
                for c2 in chars.by_ref() {
                    if ('@'..='~').contains(&c2) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    Cow::Owned(out)
}

/// Parse `name old -> new` lines, the format shared by `checkupdates`,
/// `pacman -Qu`, `paru -Qua` and `yay -Qua`. Lines flagged `[ignored]`
/// (IgnorePkg in pacman.conf) are dropped because they will not be upgraded.
pub fn parse_pacman_updates(output: &str) -> Vec<Update> {
    let clean = strip_ansi(output);
    let mut v: Vec<Update> = clean
        .lines()
        .filter_map(|line| {
            let t: Vec<&str> = line.split_whitespace().collect();
            if t.len() < 4 || t[2] != "->" {
                return None;
            }
            if t[4..].iter().any(|x| *x == "[ignored]") {
                return None;
            }
            Some(Update::new(t[0], t[1], t[3]))
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Parse `pacman -Qm` (`name version` per line).
pub fn parse_foreign_packages(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let (n, v) = (it.next()?, it.next()?);
            it.next().is_none().then(|| (n.to_string(), v.to_string()))
        })
        .collect()
}

#[derive(Deserialize)]
struct RpcResponse {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    results: Vec<RpcPackage>,
}

#[derive(Deserialize)]
struct RpcPackage {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Version")]
    version: String,
}

/// Parse an AUR RPC v5 `info` response into (name, version) pairs.
pub fn parse_aur_rpc(json: &str) -> Result<Vec<(String, String)>, ParseError> {
    let resp: RpcResponse = serde_json::from_str(json)?;
    if resp.kind == "error" {
        return Err(ParseError::AurRpc(
            resp.error.unwrap_or_else(|| "unknown error".into()),
        ));
    }
    Ok(resp.results.into_iter().map(|p| (p.name, p.version)).collect())
}

/// Compare locally installed foreign packages with AUR versions. Packages that
/// are not in the AUR (local builds) are simply absent from `remote`.
pub fn aur_updates(local: &[(String, String)], remote: &[(String, String)]) -> Vec<Update> {
    let remote: HashMap<&str, &str> = remote.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
    let mut v: Vec<Update> = local
        .iter()
        .filter_map(|(name, old)| {
            let new = remote.get(name.as_str())?;
            (vercmp(new, old) == Ordering::Greater).then(|| Update::new(name, old, *new))
        })
        .collect();
    v.sort();
    v
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatpakRef {
    /// `id/arch/branch` as printed by `--columns=ref` (no `app/` prefix).
    pub reference: String,
    pub version: String,
    pub commit: String,
}

/// Parse tab-separated `ref,version,commit` output of `flatpak list` or
/// `flatpak remote-ls --updates`. Header lines (printed when attached to a
/// TTY) are skipped.
pub fn parse_flatpak_columns(output: &str) -> Vec<FlatpakRef> {
    output
        .lines()
        .filter_map(|l| {
            let mut cols = l.split('\t');
            let reference = cols.next()?.trim();
            if reference.is_empty() || reference == "Ref" || !reference.contains('/') {
                return None;
            }
            Some(FlatpakRef {
                reference: reference.to_string(),
                version: cols.next().unwrap_or("").trim().to_string(),
                commit: cols.next().unwrap_or("").trim().to_string(),
            })
        })
        .collect()
}

/// `org.gnome.Platform/x86_64/50` -> `org.gnome.Platform//50`;
/// `org.mozilla.firefox/x86_64/stable` -> `org.mozilla.firefox`.
pub fn flatpak_display_name(reference: &str) -> String {
    let r = reference
        .strip_prefix("app/")
        .or_else(|| reference.strip_prefix("runtime/"))
        .unwrap_or(reference);
    let parts: Vec<&str> = r.split('/').collect();
    match parts.as_slice() {
        [id, _arch, branch] if *branch != "stable" && *branch != "master" => format!("{id}//{branch}"),
        [id, ..] => id.to_string(),
        [] => r.to_string(),
    }
}

fn short_commit(c: &str) -> &str {
    &c[..c.len().min(12)]
}

pub fn flatpak_updates(installed: &[FlatpakRef], remote: &[FlatpakRef]) -> Vec<Update> {
    let local: HashMap<&str, &FlatpakRef> = installed.iter().map(|r| (r.reference.as_str(), r)).collect();
    // BTreeMap dedups refs reported by both the user and system installation.
    let mut out: BTreeMap<String, Update> = BTreeMap::new();
    for r in remote {
        let inst = local.get(r.reference.as_str());
        let label = |version: &str, commit: &str| -> String {
            if !version.is_empty() {
                version.to_string()
            } else if !commit.is_empty() {
                short_commit(commit).to_string()
            } else {
                "?".to_string()
            }
        };
        let mut old = inst.map(|i| label(&i.version, &i.commit)).unwrap_or_else(|| "?".into());
        let mut new = label(&r.version, &r.commit);
        // Same version string but a new commit (rebuild): show commits too.
        // Skipped when the labels already are commits (no version column).
        if old == new {
            if let Some(i) = inst {
                if !i.version.is_empty() && !i.commit.is_empty() && !r.commit.is_empty() {
                    old = format!("{old} ({})", short_commit(&i.commit));
                    new = format!("{new} ({})", short_commit(&r.commit));
                }
            }
        }
        let name = flatpak_display_name(&r.reference);
        out.insert(r.reference.clone(), Update::new(name, old, new));
    }
    let mut v: Vec<Update> = out.into_values().collect();
    v.sort();
    v
}

/// Parse `snap list` / `snap refresh --list` tables into (name, version).
pub fn parse_snap_table(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let (n, v) = (it.next()?, it.next()?);
            (n != "Name").then(|| (n.to_string(), v.to_string()))
        })
        .collect()
}

pub fn snap_updates(installed: &[(String, String)], refreshable: &[(String, String)]) -> Vec<Update> {
    let local: HashMap<&str, &str> = installed.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
    let mut v: Vec<Update> = refreshable
        .iter()
        .map(|(n, new)| Update::new(n, local.get(n.as_str()).copied().unwrap_or("?"), new))
        .collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colour_codes() {
        assert_eq!(strip_ansi("\x1b[1mfoo\x1b[0m 1 -> 2"), "foo 1 -> 2");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn display_names() {
        assert_eq!(flatpak_display_name("org.mozilla.firefox/x86_64/stable"), "org.mozilla.firefox");
        assert_eq!(flatpak_display_name("runtime/org.gnome.Platform/x86_64/50"), "org.gnome.Platform//50");
    }
}
