//! GSettings bridge.
//!
//! Integration choice: GSettings only emits `changed` from the GLib main
//! context it was created in, and tokio has its own reactor. Rather than
//! interleaving the two loops on one thread (polling a GLib context from a
//! tokio task, or running tokio inside GLib), a dedicated OS thread owns a
//! private `glib::MainContext` + `MainLoop` and the `gio::Settings` object.
//! On every `changed` signal it reads all keys into a plain `Config`
//! snapshot and sends it over a tokio unbounded channel (sending is
//! synchronous and never blocks). No GObject ever crosses threads, and the
//! tokio side only sees `Config` values.
//!
//! The schema is loaded from the extension's `schemas/` directory with
//! `SettingsSchemaSource::from_directory`, so the daemon and the extension
//! share one source of truth without installing the schema system-wide.

use crate::config::{AurHelper, Config, Source};
use crate::{EXTENSION_UUID, SCHEMA_ID};
use gio::glib;
use gio::prelude::*;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

pub struct Loaded {
    pub initial: Config,
    /// Where the schema came from, for logs.
    pub origin: String,
    /// False when running on built-in defaults (no schema found).
    pub live: bool,
}

/// Directories searched for `gschemas.compiled`, most specific first. User
/// installs shadow system ones, matching how GNOME Shell loads extensions.
pub fn candidate_dirs() -> Vec<PathBuf> {
    let rel = Path::new("gnome-shell/extensions").join(EXTENSION_UUID).join("schemas");
    let mut v = vec![glib::user_data_dir().join(&rel)];
    v.extend(glib::system_data_dirs().into_iter().map(|d| d.join(&rel)));
    v
}

fn open_settings(explicit: Option<&Path>) -> Result<(gio::Settings, gio::SettingsSchema, String), String> {
    let default_source = gio::SettingsSchemaSource::default();
    let dirs: Vec<PathBuf> = match explicit {
        Some(d) => vec![d.to_path_buf()],
        None => candidate_dirs(),
    };
    let mut errors = Vec::new();
    for dir in &dirs {
        if !dir.join("gschemas.compiled").is_file() {
            continue;
        }
        match gio::SettingsSchemaSource::from_directory(dir, default_source.as_ref(), false) {
            // Non-recursive lookup: we want the schema from this directory.
            Ok(src) => match src.lookup(SCHEMA_ID, false) {
                Some(schema) => {
                    let settings = gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
                    return Ok((settings, schema, dir.display().to_string()));
                }
                None => errors.push(format!("{}: schema {SCHEMA_ID} not in directory", dir.display())),
            },
            Err(e) => errors.push(format!("{}: {e}", dir.display())),
        }
    }
    if explicit.is_some() {
        return Err(format!("no usable schema in {}: {}", dirs[0].display(), errors.join("; ")));
    }
    // Last resort: a schema installed into /usr/share/glib-2.0/schemas.
    if let Some(schema) = default_source.as_ref().and_then(|s| s.lookup(SCHEMA_ID, true)) {
        let settings = gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
        return Ok((settings, schema, "default schema source".into()));
    }
    Err(if errors.is_empty() {
        format!("schema {SCHEMA_ID} not found (is the GNOME extension installed?)")
    } else {
        errors.join("; ")
    })
}

/// Reads keys defensively. g_settings_get_* aborts the process on an unknown
/// key or a type mismatch, which could happen if the daemon and the
/// extension come from different versions. Each read checks the schema
/// first and falls back to the built-in default.
struct Reader<'a> {
    s: &'a gio::Settings,
    schema: &'a gio::SettingsSchema,
}

impl Reader<'_> {
    fn has(&self, key: &str, ty: &glib::VariantTy) -> bool {
        let ok = self.schema.has_key(key) && self.schema.key(key).value_type().as_str() == ty.as_str();
        if !ok {
            tracing::warn!(key, "key missing from schema or wrong type, using default");
        }
        ok
    }
    fn b(&self, key: &str, def: bool) -> bool {
        if self.has(key, glib::VariantTy::BOOLEAN) { self.s.boolean(key) } else { def }
    }
    fn u(&self, key: &str, def: u32) -> u32 {
        if self.has(key, glib::VariantTy::UINT32) { self.s.uint(key) } else { def }
    }
    fn s(&self, key: &str, def: &str) -> String {
        if self.has(key, glib::VariantTy::STRING) { self.s.string(key).to_string() } else { def.to_string() }
    }
    fn strv(&self, key: &str) -> Vec<String> {
        if self.has(key, glib::VariantTy::STRING_ARRAY) {
            self.s.strv(key).iter().map(|g| g.to_string()).collect()
        } else {
            Vec::new()
        }
    }
}

fn read_config(s: &gio::Settings, schema: &gio::SettingsSchema) -> Config {
    let r = Reader { s, schema };
    let d = Config::default();
    let mut c = Config {
        // Clamp so a hand-edited dconf value cannot cause a busy loop.
        interval: Duration::from_secs(u64::from(r.u("check-interval", 60).max(5)) * 60),
        initial_delay: Duration::from_secs(u64::from(r.u("initial-delay", 60))),
        aur_helper: AurHelper::from_key(&r.s("aur-helper", "auto")),
        unmetered_only: r.b("unmetered-only", d.unmetered_only),
        notify_enabled: r.b("notify-enabled", d.notify_enabled),
        notify_min_count: r.u("notify-min-count", d.notify_min_count),
        notify_only_increase: r.b("notify-only-increase", d.notify_only_increase),
        log_level: r.s("log-level", &d.log_level),
        ignore: r.strv("ignore-list"),
        ..d.clone()
    };
    c.terminal.enabled = r.b("terminal-enabled", d.terminal.enabled);
    c.terminal.hide_zero = r.b("terminal-hide-zero", d.terminal.hide_zero);
    c.terminal.show_breakdown = r.b("terminal-show-breakdown", d.terminal.show_breakdown);
    c.terminal.list_max = r.u("terminal-list-max", d.terminal.list_max);
    c.terminal.stale_hours = r.u("terminal-stale-hours", d.terminal.stale_hours);
    for src in Source::ALL {
        let def = d.source(src);
        let k = src.key();
        let sc = c.source_mut(src);
        sc.enabled = r.b(&format!("check-{k}"), def.enabled);
        let secs = r.u(&format!("timeout-{k}"), def.timeout.as_secs() as u32).max(10);
        sc.timeout = Duration::from_secs(u64::from(secs));
    }
    c
}

/// Start the GSettings thread and return the initial configuration. When the
/// schema cannot be found the daemon still runs, on built-in defaults.
pub fn spawn(schema_dir: Option<PathBuf>, tx: UnboundedSender<Config>) -> anyhow::Result<Loaded> {
    let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(Config, String), String>>();

    std::thread::Builder::new().name("gsettings".into()).spawn(move || {
        let ctx = glib::MainContext::new();
        let res = ctx.with_thread_default(|| {
            let (settings, schema, origin) = match open_settings(schema_dir.as_deref()) {
                Ok(v) => v,
                Err(e) => {
                    let _ = init_tx.send(Err(e));
                    return;
                }
            };
            let _ = init_tx.send(Ok((read_config(&settings, &schema), origin)));

            let schema_c = schema.clone();
            // Reading every key on each change is cheap (~25 keys) and keeps
            // the protocol trivial: the receiver always gets a full snapshot.
            settings.connect_changed(None, move |s, key| {
                tracing::debug!(key, "setting changed");
                // Send fails only when the daemon is shutting down.
                let _ = tx.send(read_config(s, &schema_c));
            });

            let main_loop = glib::MainLoop::new(Some(&ctx), false);
            main_loop.run();
            drop(settings);
        });
        if let Err(e) = res {
            tracing::error!(error = %e, "cannot acquire GLib main context for settings");
        }
    })?;

    match init_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok((initial, origin))) => Ok(Loaded { initial, origin, live: true }),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "GSettings schema unavailable, using built-in defaults");
            Ok(Loaded {
                initial: Config::default(),
                origin: "built-in defaults".into(),
                live: false,
            })
        }
        Err(e) => anyhow::bail!("settings thread did not start: {e}"),
    }
}
