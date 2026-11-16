# arch-update-notifier

Pending-update checker for Arch Linux with three front ends:

- **Daemon** (`arch-update-notifier`, Rust, systemd user service): checks the
  official repositories, the AUR, Flatpak and Snap on a schedule, writes
  `state.json` and `summary.txt`, sends desktop notifications and exposes a
  small D-Bus interface.
- **Terminal message**: a bash snippet that prints a one-line summary in
  interactive shells, using only the cached summary file.
- **GNOME Shell extension** (GNOME 50+): top bar indicator with a per-source
  package list, "Check now", "Upgrade now…" and a preferences window that
  controls everything, including the daemon.

All of it runs as the normal user. Nothing uses sudo or modifies the system
package databases: `checkupdates` syncs a private copy of the sync databases
(`$XDG_CACHE_HOME/arch-update-notifier/checkup-db`), and the AUR checks only
query.

## Layout

```
Cargo.toml, Cargo.lock        daemon crate (lib "aun" + binary)
src/                          daemon sources
  main.rs                     CLI
  daemon.rs                   main loop (tokio select)
  settings.rs                 GSettings thread, schema loading
  dbus.rs                     zbus interface
  checkers.rs                 per-source checks
  exec.rs                     subprocesses with timeouts / process groups
  parse.rs                    output parsers
  vercmp.rs                   libalpm vercmp port (AUR RPC backend)
  state.rs                    state.json, summary.txt, notification policy
  network.rs                  NetworkManager metered/offline detection
  notify.rs, logging.rs, glob.rs, config.rs
tests/parsers.rs, tests/fixtures/   parser tests with sample output
data/*.in                     systemd user unit, D-Bus activation file
shell/arch-update-notifier.bash     terminal snippet
extension/arch-update-notifier@nodefive.github.io/
  extension.js, prefs.js, lib/, icons/, stylesheet.css, metadata.json
  schemas/org.gnome.shell.extensions.arch-update-notifier.gschema.xml
packaging/arch/PKGBUILD
Makefile                      per-user install, dist tarball, lint
```

## Install

### Package (system-wide)

```sh
make dist
cd packaging/arch
makepkg -si          # builds arch-node5-update-notifier-*.pkg.tar.zst
```

The package is named `arch-node5-update-notifier`, not `arch-update-notifier`:
an unrelated AUR package already uses that name, and AUR helpers match
locally built packages by name, so they would report it as an update and
`yay -Syu` / `paru -Syu` would replace this package with it. The program,
unit, extension and paths keep the `arch-update-notifier` name. The package
conflicts with `arch-update-notifier`, so `pacman -U` offers to remove an
older install under that name.

`makepkg` checks for `cargo` as a package. If your toolchain comes from the
rustup installer script instead of the `rust` or `rustup` package, build with
`makepkg -d` and install the result with `pacman -U`.

Installed paths:

| File | Path |
| --- | --- |
| Daemon | `/usr/bin/arch-update-notifier` |
| User unit | `/usr/lib/systemd/user/arch-update-notifier.service` |
| D-Bus activation | `/usr/share/dbus-1/services/io.github.nodefive.ArchUpdateNotifier.service` |
| Shell snippet | `/usr/share/arch-update-notifier/arch-update-notifier.bash` |
| Extension | `/usr/share/gnome-shell/extensions/arch-update-notifier@nodefive.github.io/` |

### Per-user (no package)

```sh
make install        # builds with --locked, compiles the schema, installs to ~/.local and ~/.config
make uninstall
```

Paths: `~/.local/bin`, `~/.config/systemd/user`,
`~/.local/share/dbus-1/services`, `~/.local/share/arch-update-notifier`,
`~/.local/share/gnome-shell/extensions/<uuid>`. Make sure `~/.local/bin` is
not needed in `PATH` for the service: the unit uses the absolute path.

## Enable

Daemon:

```sh
systemctl --user daemon-reload
systemctl --user enable --now arch-update-notifier.service
```

The D-Bus activation file also starts the service on demand (for example
when "Check now" is clicked while it is stopped).

Extension (log out and back in first after a fresh install, Wayland does not
load new extensions into a running session):

```sh
gnome-extensions enable arch-update-notifier@nodefive.github.io
gnome-extensions prefs arch-update-notifier@nodefive.github.io
```

Terminal message, in `~/.bashrc`:

```sh
[ -r /usr/share/arch-update-notifier/arch-update-notifier.bash ] && . /usr/share/arch-update-notifier/arch-update-notifier.bash
```

(per-user install: `~/.local/share/arch-update-notifier/arch-update-notifier.bash`;
the Terminal page in the preferences shows the right line with a copy button).

Example output:

```
23 updates available (18 repo, 4 AUR, 1 flatpak) — last checked 12 min ago
warning: update information is stale (last successful check 2 days ago). Is arch-update-notifier.service running?
```

The snippet uses bash builtins only (`mapfile`, `read`, `$EPOCHSECONDS`), so it
adds no process to shell startup; measured at a few milliseconds including
bash itself. It prints once per shell tree (`AUN_MOTD_SHOWN` is exported) and
can be silenced per shell with `AUN_MOTD=0`. If `summary.txt` is missing it
falls back to one `jq` call on `state.json` when jq is installed.

## Settings

One GSettings schema, shipped in the extension's `schemas/` directory, is
used by the extension, the preferences window and the daemon. The daemon
loads it with `SettingsSchemaSource::from_directory` from (first match):

1. `--schema-dir DIR`
2. `$XDG_DATA_HOME/gnome-shell/extensions/<uuid>/schemas`
3. `$XDG_DATA_DIRS/gnome-shell/extensions/<uuid>/schemas` (`/usr/local/share`, `/usr/share`)
4. the default schema source

If none is found it runs on built-in defaults (same values as the schema)
and logs a warning. Changes apply live: the daemon subscribes to `changed`.
From the command line:

```sh
S=/usr/share/gnome-shell/extensions/arch-update-notifier@nodefive.github.io/schemas
gsettings --schemadir $S list-recursively org.gnome.shell.extensions.arch-update-notifier
gsettings --schemadir $S set org.gnome.shell.extensions.arch-update-notifier ignore-list "['linux*', 'nvidia-*']"
```

| Group | Keys |
| --- | --- |
| Checking | `check-interval` (min), `initial-delay` (s), `check-repo`, `check-aur`, `check-flatpak`, `check-snap`, `aur-helper` (auto/paru/yay/rpc), `timeout-repo/aur/flatpak/snap` (s), `unmetered-only` |
| Notifications | `notify-enabled`, `notify-min-count`, `notify-only-increase` |
| Terminal | `terminal-enabled`, `terminal-hide-zero`, `terminal-show-breakdown`, `terminal-list-max`, `terminal-stale-hours` |
| Indicator | `indicator-show-count`, `indicator-hide-zero`, `panel-position`, `icon-style` (package/arrow/theme) |
| Upgrade | `terminal-app` (auto/ptyxis/kgx/gnome-terminal/kitty/alacritty/custom), `terminal-custom`, `upgrade-commands` |
| Advanced | `log-level`, `ignore-list` (globs `*` `?`) |

## How it works

### Daemon

- Single-threaded tokio runtime. One `select!` loop handles the schedule
  timer, D-Bus `CheckNow`, settings snapshots, a 60 s watcher on
  `/var/lib/pacman/local` (re-checks after any pacman transaction), the
  running check task, and SIGTERM/SIGINT.
- GSettings integration: a dedicated thread owns a private GLib
  `MainContext` + `MainLoop` and the `gio::Settings` object. On `changed` it
  reads all keys into a plain `Config` and sends it over a tokio channel. No
  GObject crosses threads, and the two event loops never share a thread.
- Sources run concurrently, each with its own timeout. Every subprocess gets
  an argv (never a shell), `LC_ALL=C`, closed stdin and its own process group,
  which is killed on timeout or shutdown.
- Repo: `checkupdates --nocolor` with a private `CHECKUPDATES_DB`; waits up to
  2 min while `/var/lib/pacman/db.lck` exists.
- AUR: `paru -Qua` or `yay -Qua`; without a helper (or with `rpc`),
  `pacman -Qm` plus batched AUR RPC v5 `info` queries over rustls, compared
  with a port of libalpm `vercmp`.
- Flatpak: `flatpak list` joined with `flatpak remote-ls --updates`. Snap:
  `snap list` joined with `snap refresh --list`. Missing flatpak/snap binaries
  count as "nothing to update".
- A failing source keeps its previous list and is marked with an error; the
  others are unaffected. Offline (NetworkManager connectivity "none") skips
  the check and retries in 5 min; with `unmetered-only`, scheduled checks on
  metered connections are skipped (manual checks still run).
- Notifications are sent only for updates not announced before, so upgrading
  part of the list does not repeat the notification.

### Files

`$XDG_CACHE_HOME/arch-update-notifier/` (default `~/.cache/...`):

- `state.json`: full state (status, timestamps, per-source package lists,
  ignored packages, errors). Written atomically.
- `summary.txt`: pre-rendered terminal message; first line is a header
  `AUN1<TAB>enabled<TAB>last_check<TAB>last_success<TAB>stale_seconds`.
- `checkup-db/`: private sync database copy for checkupdates.

### D-Bus

```
bus name   io.github.nodefive.ArchUpdateNotifier   (session bus)
object     /io/github/nodefive/ArchUpdateNotifier
interface  io.github.nodefive.ArchUpdateNotifier1
  CheckNow() -> (b accepted)
  signal UpdatesChanged(u count, x last_check)
  properties: Count u, LastCheck x, Status s   (PropertiesChanged emitted)
```

```sh
gdbus call --session --dest io.github.nodefive.ArchUpdateNotifier \
  --object-path /io/github/nodefive/ArchUpdateNotifier \
  --method io.github.nodefive.ArchUpdateNotifier1.CheckNow
```

### Extension

The extension never runs package commands. It reads `state.json`
asynchronously on `Gio.FileMonitor` events and on `UpdatesChanged`
(debounced), watches the bus name to know whether the daemon runs, and calls
`CheckNow`. "Upgrade now…" spawns the configured terminal with
`Gio.Subprocess` running the upgrade commands in order, then calls `CheckNow`
and waits for Enter. `disable()` cancels pending I/O, removes timeouts,
unsubscribes from D-Bus, cancels the monitor and destroys the indicator.

## Development

```sh
cargo test --locked            # parser, vercmp, state and policy tests
make lint                      # bash -n, shellcheck, JS syntax, schema check
make schemas                   # compile the schema for a source-tree run
target/release/arch-update-notifier --once --schema-dir extension/arch-update-notifier@nodefive.github.io/schemas
```

`--once` runs one check, prints the terminal message and exits (no D-Bus,
no notification). `--stderr` logs to stderr instead of the journal.

## Troubleshooting

Daemon logs and state:

```sh
systemctl --user status arch-update-notifier
journalctl --user -u arch-update-notifier -f
cat ~/.cache/arch-update-notifier/state.json
```

Raise the log level in Settings › Advanced (applies live), or run in the
foreground: `systemctl --user stop arch-update-notifier && arch-update-notifier --stderr`.

Extension:

```sh
gnome-extensions info arch-update-notifier@nodefive.github.io
gnome-extensions list --enabled
journalctl --user -b -g arch-update-notifier /usr/bin/gnome-shell
```

Nested session for testing (no logout needed). GNOME 49 and later replaced
`--nested` with the development kit viewer (`mutter-devkit`):

```sh
dbus-run-session gnome-shell --devkit          # GNOME 49+
dbus-run-session gnome-shell --nested --wayland  # GNOME 48 and older
```

Inside it, enable the extension with `gnome-extensions enable ...` from a
terminal opened in that session. For scripted tests a headless instance also
works: `dbus-run-session -- gnome-shell --headless --virtual-monitor 1280x800 --no-x11`.

Common problems:

| Symptom | Cause / fix |
| --- | --- |
| Indicator shows the error icon, "daemon is not running" | `systemctl --user enable --now arch-update-notifier` |
| "settings: using built-in defaults" in the journal | the extension (and its compiled schema) is not installed where the daemon looks; use `--schema-dir` in a drop-in (`systemctl --user edit arch-update-notifier`) |
| Repo check: "checkupdates failed ... Cannot fetch updates" | no network or mirror problem; the previous list is kept |
| Repo check: "pacman database is locked" | a transaction was running for more than 2 min, or a stale `/var/lib/pacman/db.lck` |
| AUR always via RPC | paru/yay not in the service's `PATH`; the unit inherits the user manager environment (`systemctl --user show-environment`) |
| No terminal message | `terminal-enabled` off, `hide-zero` with 0 updates, `AUN_MOTD_SHOWN` inherited (e.g. tmux started from a shell), or `summary.txt` missing |

## License

GPL-2.0-or-later.
