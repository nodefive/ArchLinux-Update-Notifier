//! Parser tests against captured/representative command output.

use aun::parse::*;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/", $name))
    };
}

fn names(v: &[Update]) -> Vec<&str> {
    v.iter().map(|u| u.name.as_str()).collect()
}

#[test]
fn checkupdates_output() {
    let v = parse_pacman_updates(fixture!("checkupdates.txt"));
    // nvidia-utils is flagged [ignored] (IgnorePkg) and must be dropped.
    assert_eq!(names(&v), ["firefox", "linux", "linux-headers", "mesa", "python-gobject"]);
    let mesa = v.iter().find(|u| u.name == "mesa").expect("mesa parsed");
    assert_eq!(mesa.old, "1:25.2.6-1");
    assert_eq!(mesa.new, "1:25.2.7-1");
}

#[test]
fn checkupdates_empty() {
    assert!(parse_pacman_updates("").is_empty());
    assert!(parse_pacman_updates("\n\n").is_empty());
}

#[test]
fn paru_output_with_colour_and_warnings() {
    let v = parse_pacman_updates(fixture!("paru_qua.txt"));
    assert_eq!(names(&v), ["paru", "visual-studio-code-bin", "zen-browser-bin"]);
    assert_eq!(v[0].old, "2.1.0-1");
    assert_eq!(v[0].new, "2.1.1-1");
}

#[test]
fn yay_output_with_noise_lines() {
    let v = parse_pacman_updates(fixture!("yay_qua.txt"));
    assert_eq!(names(&v), ["google-chrome", "neovim-git", "yay-bin"]);
    assert_eq!(v[1].new, "latest-commit");
}

#[test]
fn aur_rpc_fallback() {
    let local = parse_foreign_packages(fixture!("pacman_qm.txt"));
    assert_eq!(local.len(), 5);
    let remote = parse_aur_rpc(fixture!("aur_rpc.json")).expect("valid RPC response");
    assert_eq!(remote.len(), 4);
    let v = aur_updates(&local, &remote);
    // google-chrome and yay-bin are newer; spotify equal; zoom older in the
    // AUR (no downgrade); my-local-build is not in the AUR.
    assert_eq!(names(&v), ["google-chrome", "yay-bin"]);
}

#[test]
fn aur_rpc_error_and_garbage() {
    let e = parse_aur_rpc(fixture!("aur_rpc_error.json")).unwrap_err();
    assert!(e.to_string().contains("Too many package results"));
    assert!(parse_aur_rpc("<html>502 Bad Gateway</html>").is_err());
}

#[test]
fn flatpak_updates_join_and_dedup() {
    let installed = parse_flatpak_columns(fixture!("flatpak_list.txt"));
    let remote = parse_flatpak_columns(fixture!("flatpak_remote_ls.txt"));
    assert_eq!(installed.len(), 4);
    assert_eq!(remote.len(), 4, "header skipped, duplicate kept until join");
    let v = flatpak_updates(&installed, &remote);
    assert_eq!(names(&v), ["org.freedesktop.Platform.GL.default//25.08", "org.gnome.Platform//50", "org.mozilla.firefox"]);

    let ff = v.iter().find(|u| u.name == "org.mozilla.firefox").expect("firefox");
    assert_eq!((ff.old.as_str(), ff.new.as_str()), ("145.0", "145.0.1"));
    // Same version, new commit: commits are appended.
    let gp = v.iter().find(|u| u.name == "org.gnome.Platform//50").expect("platform");
    assert_eq!((gp.old.as_str(), gp.new.as_str()), ("50.1 (aaaaaaaaaaaa)", "50.1 (bbbbbbbbbbbb)"));
    // No version at all: abbreviated commits.
    let gl = &v[0];
    assert_eq!((gl.old.as_str(), gl.new.as_str()), ("0001deadbeef", "0002deadbeef"));
}

#[test]
fn flatpak_empty() {
    assert!(flatpak_updates(&[], &parse_flatpak_columns("")).is_empty());
}

#[test]
fn snap_updates_from_tables() {
    let installed = parse_snap_table(fixture!("snap_list.txt"));
    let pending = parse_snap_table(fixture!("snap_refresh.txt"));
    let v = snap_updates(&installed, &pending);
    assert_eq!(names(&v), ["firefox", "snapd"]);
    assert_eq!((v[0].old.as_str(), v[0].new.as_str()), ("144.0.2-1", "145.0-1"));
}
