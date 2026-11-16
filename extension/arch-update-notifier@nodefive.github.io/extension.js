// Arch Update Notifier - GNOME Shell top bar indicator.
//
// This extension never runs package commands. It only reads the daemon's
// state.json (asynchronously, on Gio.FileMonitor events and on the D-Bus
// UpdatesChanged signal) and calls the daemon's CheckNow method. The only
// process it spawns is the user's terminal for "Upgrade now...", via
// Gio.Subprocess (asynchronous, never waited on synchronously).

import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Gio from 'gi://Gio';
import Pango from 'gi://Pango';
import St from 'gi://St';

import {Extension, gettext as _, ngettext} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import {BUS_NAME, INTERFACE, OBJECT_PATH, SOURCES, formatAgo, parseState, stateFilePath} from './lib/state.js';
import {buildUpgradeArgv} from './lib/terminal.js';

// Rows rendered per source; the rest is summarised to keep the menu fast.
const MAX_ROWS_PER_SOURCE = 300;
// Coalesces bursts of file monitor events (write + rename) into one read.
const RELOAD_DEBOUNCE_MS = 250;
// Give up on the "checking" spinner if the daemon never reports back.
const CHECK_PENDING_TIMEOUT_S = 120;

const THEME_ICONS = {
    ok: 'object-select-symbolic',
    updates: 'software-update-available-symbolic',
    checking: 'view-refresh-symbolic',
    error: 'dialog-warning-symbolic',
};

/** Clickable section header that expands/collapses its package list. */
const SourceHeader = GObject.registerClass(
class SourceHeader extends PopupMenu.PopupBaseMenuItem {
    _init(title, expanded, onToggle) {
        super._init({style_class: 'aun-source-header'});
        this._expanded = expanded;
        this._onToggle = onToggle;

        this._arrow = new St.Icon({
            style_class: 'popup-menu-arrow',
            y_align: Clutter.ActorAlign.CENTER,
        });
        this.add_child(this._arrow);

        this.label = new St.Label({text: title, x_expand: true, y_align: Clutter.ActorAlign.CENTER});
        this.add_child(this.label);
        this.label_actor = this.label;
        this._syncArrow();
    }

    _syncArrow() {
        this._arrow.icon_name = this._expanded ? 'pan-down-symbolic' : 'pan-end-symbolic';
    }

    // Toggle instead of emitting 'activate', so the menu stays open (same
    // approach as PopupSubMenuMenuItem). Also used for keyboard activation.
    activate(_event) {
        this._expanded = !this._expanded;
        this._syncArrow();
        this._onToggle(this._expanded);
    }
});

/** Two-line, non-interactive header: total and last-check time. */
const MenuHeader = GObject.registerClass(
class MenuHeader extends PopupMenu.PopupBaseMenuItem {
    _init() {
        super._init({reactive: false, can_focus: false, style_class: 'aun-header'});
        const box = new St.BoxLayout({orientation: Clutter.Orientation.VERTICAL, x_expand: true});
        this.title = new St.Label({style_class: 'aun-header-title'});
        this.subtitle = new St.Label({style_class: 'aun-header-subtitle'});
        box.add_child(this.title);
        box.add_child(this.subtitle);
        this.add_child(box);
    }
});

const Indicator = GObject.registerClass(
class Indicator extends PanelMenu.Button {
    _init(ext, settings) {
        super._init(0.5, _('Arch Update Notifier'));
        this._ext = ext;
        this._settings = settings;
        this._model = null;
        this._dirty = true;
        this._expanded = new Map();
        this._iconCache = new Map();

        const box = new St.BoxLayout({style_class: 'panel-status-indicators-box'});
        this._icon = new St.Icon({style_class: 'system-status-icon aun-icon'});
        this._count = new St.Label({
            style_class: 'aun-count',
            y_align: Clutter.ActorAlign.CENTER,
            visible: false,
        });
        box.add_child(this._icon);
        box.add_child(this._count);
        this.add_child(box);

        // Menu layout: header, messages, scrollable per-source lists, actions.
        this._header = new MenuHeader();
        this.menu.addMenuItem(this._header);

        this._messages = new PopupMenu.PopupMenuSection();
        this.menu.addMenuItem(this._messages);

        // The list section lives inside a ScrollView capped by CSS max-height.
        this._listSection = new PopupMenu.PopupMenuSection();
        this._scroll = new St.ScrollView({
            style_class: 'aun-scroll',
            hscrollbar_policy: St.PolicyType.NEVER,
            vscrollbar_policy: St.PolicyType.AUTOMATIC,
            overlay_scrollbars: true,
        });
        this._scroll.set_child(this._listSection.actor);
        const scrollHolder = new PopupMenu.PopupMenuSection();
        scrollHolder.actor.add_child(this._scroll);
        this.menu.addMenuItem(scrollHolder);

        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this._checkItem = this.menu.addAction(_('Check now'),
            () => this._ext.checkNow(), 'view-refresh-symbolic');
        this.menu.addAction(_('Upgrade now…'),
            () => this._ext.upgradeNow(), 'software-update-available-symbolic');
        this.menu.addAction(_('Settings'),
            () => this._ext.openPreferences(), 'preferences-system-symbolic');

        // Build the (possibly long) lists lazily, only when the menu opens.
        this.menu.connectObject('open-state-changed', (_m, open) => {
            if (!open)
                return;
            if (this._dirty)
                this._rebuildMenu();
            else
                this._updateHeader();
        }, this);
    }

    /**
     * @param {object} model {state, loadError, daemonUp, checkPending, actionError}
     */
    update(model) {
        this._model = model;
        const settings = this._settings;
        const {state} = model;
        const count = state?.count ?? 0;
        const mode = this._mode(model);

        this._icon.gicon = this._gicon(settings.get_string('icon-style'), mode);
        for (const m of ['ok', 'updates', 'checking', 'error']) {
            if (m === mode)
                this.add_style_class_name(`aun-state-${m}`);
            else
                this.remove_style_class_name(`aun-state-${m}`);
        }

        this._count.text = String(count);
        this._count.visible = settings.get_boolean('indicator-show-count') && count > 0;

        // Hide only when there is nothing to report; errors stay visible.
        this.visible = !(settings.get_boolean('indicator-hide-zero') && count === 0 && mode !== 'error');

        this._dirty = true;
        if (this.menu.isOpen)
            this._rebuildMenu();
    }

    _mode({state, loadError, daemonUp, checkPending}) {
        if (checkPending || state?.status === 'checking')
            return 'checking';
        if (state?.count > 0)
            return 'updates';
        if (loadError || state?.status === 'error' || (!state && !daemonUp))
            return 'error';
        return 'ok';
    }

    _gicon(style, mode) {
        const key = `${style}:${mode}`;
        let icon = this._iconCache.get(key);
        if (!icon) {
            if (style === 'theme') {
                icon = Gio.ThemedIcon.new_with_default_fallbacks(THEME_ICONS[mode]);
            } else {
                const set = style === 'arrow' ? 'arrow' : 'package';
                // "-symbolic.svg" file icons are recoloured like theme icons.
                const path = GLib.build_filenamev([this._ext.path, 'icons', `aun-${set}-${mode}-symbolic.svg`]);
                icon = new Gio.FileIcon({file: Gio.File.new_for_path(path)});
            }
            this._iconCache.set(key, icon);
        }
        return icon;
    }

    _updateHeader() {
        const {state, loadError, daemonUp, checkPending} = this._model ?? {};
        const count = state?.count ?? 0;
        let title;
        if (!state)
            title = loadError ? _('Cannot read update status') : _('No update information yet');
        else if (count === 0)
            title = _('System is up to date');
        else
            title = ngettext('%d update available', '%d updates available', count).format(count);
        this._header.title.text = title;

        let sub;
        if (checkPending || state?.status === 'checking')
            sub = _('Checking for updates…');
        else if (!state && !daemonUp)
            sub = _('The daemon is not running. Use “Check now” or start it in Settings › Advanced.');
        else if (state)
            sub = _('Last checked %s').format(formatAgo(state.lastCheck, _, ngettext));
        else
            sub = '';
        if (state?.ignoredCount > 0)
            sub += `  ·  ${ngettext('%d ignored', '%d ignored', state.ignoredCount).format(state.ignoredCount)}`;
        this._header.subtitle.text = sub;
        this._checkItem.setSensitive(!(checkPending || state?.status === 'checking'));
    }

    _addMessage(text, styleClass) {
        const item = new PopupMenu.PopupMenuItem(text, {reactive: false, can_focus: false, style_class: styleClass});
        item.label.clutter_text.line_wrap = true;
        item.label.clutter_text.ellipsize = Pango.EllipsizeMode.NONE;
        this._messages.addMenuItem(item);
    }

    _rebuildMenu() {
        this._dirty = false;
        this._updateHeader();
        this._messages.removeAll();
        this._listSection.removeAll();
        // removeAll() only destroys menu items; the package list boxes are
        // plain actors in the same container and must be removed as well.
        this._listSection.actor.destroy_all_children();

        const {state, loadError, actionError} = this._model ?? {};
        if (loadError)
            this._addMessage(loadError, 'aun-message-error');
        if (actionError)
            this._addMessage(actionError, 'aun-message-error');
        if (state?.status === 'skipped' && state.message)
            this._addMessage(_('Last check skipped: %s').format(state.message), 'aun-message');
        for (const {key, title} of SOURCES) {
            const err = state?.sources[key]?.error;
            if (err)
                this._addMessage(_('%s: %s').format(title, err), 'aun-message-error');
        }

        if (!state)
            return;
        for (const {key, title} of SOURCES) {
            const src = state.sources[key];
            if (!src || src.packages.length === 0)
                continue;
            this._addSource(key, title, src.packages);
        }
    }

    _addSource(key, title, packages) {
        const expanded = this._expanded.get(key) ?? packages.length <= 15;
        const body = new St.BoxLayout({
            orientation: Clutter.Orientation.VERTICAL,
            style_class: 'aun-package-list',
            visible: expanded,
        });
        const header = new SourceHeader(`${title} (${packages.length})`, expanded, open => {
            this._expanded.set(key, open);
            body.visible = open;
        });
        this._listSection.addMenuItem(header);

        for (const p of packages.slice(0, MAX_ROWS_PER_SOURCE)) {
            const row = new St.BoxLayout({style_class: 'aun-package-row'});
            const name = new St.Label({text: p.name, x_expand: true, style_class: 'aun-package-name'});
            name.clutter_text.ellipsize = Pango.EllipsizeMode.END;
            const ver = new St.Label({text: `${p.old}  →  ${p.new}`, style_class: 'aun-package-version'});
            ver.clutter_text.ellipsize = Pango.EllipsizeMode.MIDDLE;
            row.add_child(name);
            row.add_child(ver);
            body.add_child(row);
        }
        if (packages.length > MAX_ROWS_PER_SOURCE) {
            const more = packages.length - MAX_ROWS_PER_SOURCE;
            body.add_child(new St.Label({
                text: ngettext('… and %d more', '… and %d more', more).format(more),
                style_class: 'aun-package-more',
            }));
        }
        this._listSection.actor.add_child(body);
    }

    destroy() {
        this.menu.disconnectObject(this);
        this._iconCache.clear();
        this._ext = null;
        this._settings = null;
        super.destroy();
    }
});

export default class ArchUpdateNotifierExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._cancellable = new Gio.Cancellable();
        this._state = null;
        this._loadError = null;
        this._actionError = null;
        this._daemonUp = false;
        this._checkPending = false;
        this._checkRequestedAt = 0;
        this._reloadId = 0;
        this._pendingTimeoutId = 0;

        this._createIndicator();

        this._settings.connectObject(
            'changed::panel-position', () => this._recreateIndicator(),
            'changed::icon-style', () => this._render(),
            'changed::indicator-show-count', () => this._render(),
            'changed::indicator-hide-zero', () => this._render(),
            this);

        // Watch the file even if it (or its directory) does not exist yet:
        // GLib's inotify backend tracks missing paths.
        this._stateFile = Gio.File.new_for_path(stateFilePath());
        try {
            this._monitor = this._stateFile.monitor_file(Gio.FileMonitorFlags.NONE, this._cancellable);
            this._monitor.connectObject('changed', (_m, _f, _o, event) => {
                if (event !== Gio.FileMonitorEvent.ATTRIBUTE_CHANGED)
                    this._scheduleReload();
            }, this);
        } catch (e) {
            console.warn(`[arch-update-notifier] cannot monitor ${stateFilePath()}: ${e.message}`);
            this._monitor = null;
        }

        this._signalId = Gio.DBus.session.signal_subscribe(
            BUS_NAME, INTERFACE, 'UpdatesChanged', OBJECT_PATH, null,
            Gio.DBusSignalFlags.NONE, () => this._scheduleReload());

        this._watchId = Gio.bus_watch_name(Gio.BusType.SESSION, BUS_NAME,
            Gio.BusNameWatcherFlags.NONE,
            () => {
                this._daemonUp = true;
                this._scheduleReload();
            },
            () => {
                this._daemonUp = false;
                this._render();
            });

        this._scheduleReload();
    }

    disable() {
        this._cancellable.cancel();
        this._cancellable = null;

        if (this._reloadId) {
            GLib.source_remove(this._reloadId);
            this._reloadId = 0;
        }
        if (this._pendingTimeoutId) {
            GLib.source_remove(this._pendingTimeoutId);
            this._pendingTimeoutId = 0;
        }
        if (this._signalId) {
            Gio.DBus.session.signal_unsubscribe(this._signalId);
            this._signalId = 0;
        }
        if (this._watchId) {
            Gio.bus_unwatch_name(this._watchId);
            this._watchId = 0;
        }
        if (this._monitor) {
            this._monitor.disconnectObject(this);
            this._monitor.cancel();
            this._monitor = null;
        }
        this._settings.disconnectObject(this);
        this._settings = null;

        this._indicator?.destroy();
        this._indicator = null;
        this._stateFile = null;
        this._state = null;
    }

    _createIndicator() {
        this._indicator = new Indicator(this, this._settings);
        const pos = this._settings.get_string('panel-position');
        const box = ['left', 'center', 'right'].includes(pos) ? pos : 'right';
        // Index 0 puts it next to the system menu on the right, and at the
        // start of the left/center boxes.
        Main.panel.addToStatusArea(this.uuid, this._indicator, box === 'right' ? 0 : -1, box);
        this._render();
    }

    _recreateIndicator() {
        this._indicator?.destroy();
        this._indicator = null;
        this._createIndicator();
    }

    _render() {
        this._indicator?.update({
            state: this._state,
            loadError: this._loadError,
            daemonUp: this._daemonUp,
            checkPending: this._checkPending,
            actionError: this._actionError,
        });
    }

    _scheduleReload() {
        if (this._reloadId)
            return;
        this._reloadId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, RELOAD_DEBOUNCE_MS, () => {
            this._reloadId = 0;
            this._loadState();
            return GLib.SOURCE_REMOVE;
        });
    }

    _loadState() {
        const cancellable = this._cancellable;
        this._stateFile.load_contents_async(cancellable, (file, res) => {
            let text = null;
            let error = null;
            try {
                const [, bytes] = file.load_contents_finish(res);
                text = new TextDecoder().decode(bytes);
            } catch (e) {
                if (e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                    return;
                // A missing file just means the daemon has not run yet.
                if (!e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.NOT_FOUND))
                    error = _('Cannot read the state file: %s').format(e.message);
            }
            // Disabled (or re-enabled) while reading.
            if (cancellable.is_cancelled() || !this._indicator)
                return;

            if (text !== null) {
                const parsed = parseState(text);
                if (parsed.ok) {
                    this._state = parsed.state;
                    this._loadError = null;
                } else {
                    // Keep the last good state; show what went wrong.
                    this._loadError = parsed.error;
                }
            } else {
                this._state = null;
                this._loadError = error;
            }

            if (this._checkPending && this._state &&
                (this._state.status === 'checking' || this._state.lastCheck >= this._checkRequestedAt))
                this._clearCheckPending();
            this._render();
        });
    }

    checkNow() {
        this._actionError = null;
        this._checkPending = true;
        this._checkRequestedAt = Math.floor(GLib.get_real_time() / 1e6);
        if (!this._pendingTimeoutId) {
            this._pendingTimeoutId = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, CHECK_PENDING_TIMEOUT_S, () => {
                this._pendingTimeoutId = 0;
                this._checkPending = false;
                this._render();
                return GLib.SOURCE_REMOVE;
            });
        }
        this._render();

        // With the D-Bus activation file installed this also starts the
        // daemon when it is not running.
        const cancellable = this._cancellable;
        Gio.DBus.session.call(BUS_NAME, OBJECT_PATH, INTERFACE, 'CheckNow', null,
            new GLib.VariantType('(b)'), Gio.DBusCallFlags.NONE, 30000, cancellable,
            (conn, res) => {
                try {
                    conn.call_finish(res);
                } catch (e) {
                    if (e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED) || cancellable.is_cancelled())
                        return;
                    this._clearCheckPending();
                    // Strips the "GDBus.Error:org.freedesktop..." prefix in place.
                    if (e instanceof GLib.Error)
                        Gio.DBusError.strip_remote_error(e);
                    this._actionError = _('Cannot reach the update daemon: %s').format(e.message);
                    this._render();
                }
            });
    }

    _clearCheckPending() {
        this._checkPending = false;
        if (this._pendingTimeoutId) {
            GLib.source_remove(this._pendingTimeoutId);
            this._pendingTimeoutId = 0;
        }
    }

    upgradeNow() {
        let argv;
        try {
            argv = buildUpgradeArgv(this._settings);
        } catch (e) {
            Main.notifyError(_('Cannot start the upgrade'), e.message);
            return;
        }
        try {
            // Asynchronous spawn; the terminal runs independently.
            const proc = Gio.Subprocess.new(argv, Gio.SubprocessFlags.NONE);
            proc.wait_check_async(this._cancellable, (p, res) => {
                try {
                    p.wait_check_finish(res);
                } catch (e) {
                    if (!e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                        console.warn(`[arch-update-notifier] terminal exited with error: ${e.message}`);
                }
            });
        } catch (e) {
            Main.notifyError(_('Cannot start the terminal'), `${argv[0]}: ${e.message}`);
        }
    }
}
