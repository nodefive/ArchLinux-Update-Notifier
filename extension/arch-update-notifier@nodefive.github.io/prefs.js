// Preferences window (libadwaita). Every key of the shared GSettings schema
// is exposed here; the daemon picks changes up live through its own
// GSettings `changed` subscription.

import Adw from 'gi://Adw';
import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import Gtk from 'gi://Gtk';

import {ExtensionPreferences, gettext as _, ngettext} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

import {BUS_NAME, INTERFACE, OBJECT_PATH, SERVICE_UNIT, formatAgo, parseState, stateFilePath} from './lib/state.js';
import {SELECTABLE, TERMINALS, defaultUpgradeCommands, detectTerminal} from './lib/terminal.js';

// ---------------------------------------------------------------- helpers

function switchRow(settings, key, title, subtitle = '') {
    const row = new Adw.SwitchRow({title, subtitle});
    settings.bind(key, row, 'active', Gio.SettingsBindFlags.DEFAULT);
    return row;
}

// SpinRow.value is a double; GSettings maps it to the 'u' keys.
function spinRow(settings, key, title, subtitle, lower, upper, step = 1) {
    const row = new Adw.SpinRow({
        title,
        subtitle,
        adjustment: new Gtk.Adjustment({lower, upper, step_increment: step, page_increment: step * 10}),
    });
    settings.bind(key, row, 'value', Gio.SettingsBindFlags.DEFAULT);
    return row;
}

/**
 * ComboRow bound to a string key with <choices>.
 *
 * @param {Gio.Settings} settings settings
 * @param {string} key key
 * @param {string} title row title
 * @param {string} subtitle row subtitle
 * @param {Array<[string, string]>} choices [value, label] pairs
 * @returns {Adw.ComboRow}
 */
function comboRow(settings, key, title, subtitle, choices) {
    const row = new Adw.ComboRow({
        title,
        subtitle,
        model: Gtk.StringList.new(choices.map(c => c[1])),
    });
    const sync = () => {
        const i = choices.findIndex(c => c[0] === settings.get_string(key));
        row.selected = i >= 0 ? i : 0;
    };
    sync();
    row.connect('notify::selected', () => {
        const value = choices[row.selected]?.[0];
        if (value !== undefined && value !== settings.get_string(key))
            settings.set_string(key, value);
    });
    const id = settings.connect(`changed::${key}`, sync);
    row.connect('destroy', () => settings.disconnect(id));
    return row;
}

/**
 * ActionRow showing literal text. use_markup must be off before the text is
 * set (construct properties are applied in no particular order), otherwise
 * characters like & or < are parsed as Pango markup.
 */
function plainRow(title, subtitle = '') {
    const row = new Adw.ActionRow({use_markup: false});
    row.title = title;
    row.subtitle = subtitle;
    return row;
}

/**
 * Editable list for an 'as' key: one row per entry with remove (and
 * optionally move-up) buttons, plus an entry row to add items.
 */
function stringListGroup(settings, key, {title, description, addTitle, ordered = false, emptyText = null}) {
    const group = new Adw.PreferencesGroup({title, description});
    const addRow = new Adw.EntryRow({title: addTitle, show_apply_button: true});
    addRow.connect('apply', () => {
        const text = addRow.text.trim();
        if (!text)
            return;
        const values = settings.get_strv(key);
        if (!values.includes(text))
            settings.set_strv(key, [...values, text]);
        addRow.text = '';
    });

    let rows = [];
    let addRowShown = false;
    const rebuild = () => {
        for (const r of rows)
            group.remove(r);
        rows = [];
        // Re-added below so the entry row always stays last.
        if (addRowShown)
            group.remove(addRow);

        const values = settings.get_strv(key);
        if (values.length === 0 && emptyText) {
            const r = plainRow(emptyText());
            r.add_css_class('dim-label');
            rows.push(r);
            group.add(r);
        }
        values.forEach((value, i) => {
            const r = plainRow(value);
            r.add_css_class('monospace');
            if (ordered && i > 0) {
                const up = new Gtk.Button({
                    icon_name: 'go-up-symbolic',
                    valign: Gtk.Align.CENTER,
                    tooltip_text: _('Move up'),
                    css_classes: ['flat'],
                });
                up.connect('clicked', () => {
                    const v = settings.get_strv(key);
                    [v[i - 1], v[i]] = [v[i], v[i - 1]];
                    settings.set_strv(key, v);
                });
                r.add_suffix(up);
            }
            const del = new Gtk.Button({
                icon_name: 'user-trash-symbolic',
                valign: Gtk.Align.CENTER,
                tooltip_text: _('Remove'),
                css_classes: ['flat'],
            });
            del.connect('clicked', () => {
                settings.set_strv(key, settings.get_strv(key).filter((_v, j) => j !== i));
            });
            r.add_suffix(del);
            rows.push(r);
            group.add(r);
        });
        group.add(addRow);
        addRowShown = true;
    };
    rebuild();
    const id = settings.connect(`changed::${key}`, rebuild);
    group.connect('destroy', () => settings.disconnect(id));
    return group;
}

/**
 * Run a command asynchronously and collect its output.
 *
 * @param {string[]} argv command
 * @returns {Promise<{ok: boolean, stdout: string, stderr: string}>}
 */
function run(argv) {
    return new Promise(resolve => {
        let proc;
        try {
            proc = Gio.Subprocess.new(argv,
                Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_PIPE);
        } catch (e) {
            resolve({ok: false, stdout: '', stderr: e.message});
            return;
        }
        proc.communicate_utf8_async(null, null, (p, res) => {
            try {
                const [, stdout, stderr] = p.communicate_utf8_finish(res);
                resolve({ok: p.get_successful(), stdout: stdout ?? '', stderr: stderr ?? ''});
            } catch (e) {
                resolve({ok: false, stdout: '', stderr: e.message});
            }
        });
    });
}

function loadStateFile() {
    return new Promise(resolve => {
        Gio.File.new_for_path(stateFilePath()).load_contents_async(null, (f, res) => {
            try {
                const [, bytes] = f.load_contents_finish(res);
                const parsed = parseState(new TextDecoder().decode(bytes));
                resolve(parsed.ok ? parsed.state : null);
            } catch {
                resolve(null);
            }
        });
    });
}

function toast(window, title) {
    window.add_toast(new Adw.Toast({title, timeout: 4}));
}

// ---------------------------------------------------------------- pages

function checkingPage(s) {
    const page = new Adw.PreferencesPage({title: _('Checking'), icon_name: 'view-refresh-symbolic'});

    const sched = new Adw.PreferencesGroup({title: _('Schedule')});
    sched.add(spinRow(s, 'check-interval', _('Check interval'), _('Minutes between checks'), 5, 1440, 5));
    sched.add(spinRow(s, 'initial-delay', _('Initial delay'), _('Seconds to wait after the daemon starts'), 0, 3600, 10));
    sched.add(switchRow(s, 'unmetered-only', _('Only on unmetered connections'),
        _('Skip scheduled checks on metered networks (NetworkManager). “Check now” always runs.')));
    page.add(sched);

    const sources = new Adw.PreferencesGroup({title: _('Sources')});
    sources.add(switchRow(s, 'check-repo', _('Official repositories'), _('checkupdates, using a private copy of the sync databases')));
    sources.add(switchRow(s, 'check-aur', _('AUR')));
    sources.add(comboRow(s, 'aur-helper', _('AUR backend'), _('Automatic tries paru, then yay, then the AUR RPC'), [
        ['auto', _('Automatic')],
        ['paru', 'paru'],
        ['yay', 'yay'],
        ['rpc', _('AUR RPC (no helper)')],
    ]));
    sources.add(switchRow(s, 'check-flatpak', _('Flatpak')));
    sources.add(switchRow(s, 'check-snap', _('Snap')));
    page.add(sources);

    const timeouts = new Adw.PreferencesGroup({
        title: _('Timeouts'),
        description: _('A source that exceeds its timeout keeps its previous results and is marked as failed.'),
    });
    timeouts.add(spinRow(s, 'timeout-repo', _('Official repositories'), _('Seconds'), 10, 3600, 10));
    timeouts.add(spinRow(s, 'timeout-aur', _('AUR'), _('Seconds'), 10, 3600, 10));
    timeouts.add(spinRow(s, 'timeout-flatpak', _('Flatpak'), _('Seconds'), 10, 3600, 10));
    timeouts.add(spinRow(s, 'timeout-snap', _('Snap'), _('Seconds'), 10, 3600, 10));
    page.add(timeouts);
    return page;
}

function notificationsPage(s) {
    const page = new Adw.PreferencesPage({title: _('Notifications'), icon_name: 'preferences-system-notifications-symbolic'});
    const group = new Adw.PreferencesGroup({
        description: _('Notifications are sent by the daemon when updates not announced before appear.'),
    });
    const enabled = switchRow(s, 'notify-enabled', _('Desktop notifications'));
    const min = spinRow(s, 'notify-min-count', _('Minimum number of updates'), '', 1, 1000);
    const inc = switchRow(s, 'notify-only-increase', _('Only when the count increases'),
        _('Stay quiet when new updates replace upgraded ones without the total growing'));
    for (const r of [min, inc])
        s.bind('notify-enabled', r, 'sensitive', Gio.SettingsBindFlags.GET);
    group.add(enabled);
    group.add(min);
    group.add(inc);
    page.add(group);
    return page;
}

function snippetPath() {
    const user = GLib.build_filenamev([GLib.get_user_data_dir(), 'arch-update-notifier', 'arch-update-notifier.bash']);
    if (GLib.file_test(user, GLib.FileTest.EXISTS))
        return user.replace(GLib.get_home_dir(), '~');
    return '/usr/share/arch-update-notifier/arch-update-notifier.bash';
}

function terminalPage(s, window) {
    const page = new Adw.PreferencesPage({title: _('Terminal'), icon_name: 'utilities-terminal-symbolic'});

    const group = new Adw.PreferencesGroup({
        title: _('Login message'),
        description: _('Printed by interactive bash shells from a summary file the daemon writes; adds no network or pacman calls to shell startup.'),
    });
    const enabled = switchRow(s, 'terminal-enabled', _('Show message in terminals'));
    const rows = [
        switchRow(s, 'terminal-hide-zero', _('Print nothing when up to date')),
        switchRow(s, 'terminal-show-breakdown', _('Per-source breakdown'), _('e.g. “(18 repo, 4 AUR, 1 flatpak)”')),
        spinRow(s, 'terminal-list-max', _('List package names up to'), _('Number of updates (0 = never list)'), 0, 100),
        spinRow(s, 'terminal-stale-hours', _('Staleness warning after'), _('Hours since the last successful check (0 = off)'), 0, 8760),
    ];
    group.add(enabled);
    for (const r of rows) {
        s.bind('terminal-enabled', r, 'sensitive', Gio.SettingsBindFlags.GET);
        group.add(r);
    }
    page.add(group);

    const setup = new Adw.PreferencesGroup({title: _('Setup')});
    const line = `[ -r ${snippetPath()} ] && . ${snippetPath()}`;
    const row = plainRow(_('Add to ~/.bashrc'), line);
    row.subtitle_selectable = true;
    row.add_css_class('property');
    const copy = new Gtk.Button({icon_name: 'edit-copy-symbolic', valign: Gtk.Align.CENTER, css_classes: ['flat'], tooltip_text: _('Copy')});
    copy.connect('clicked', () => {
        copy.get_clipboard().set(line);
        toast(window, _('Copied to clipboard'));
    });
    row.add_suffix(copy);
    setup.add(row);
    page.add(setup);
    return page;
}

function indicatorPage(s) {
    const page = new Adw.PreferencesPage({title: _('Indicator'), icon_name: 'preferences-desktop-display-symbolic'});
    const group = new Adw.PreferencesGroup();
    group.add(switchRow(s, 'indicator-show-count', _('Show update count')));
    group.add(switchRow(s, 'indicator-hide-zero', _('Hide when up to date'), _('Errors keep the indicator visible')));
    group.add(comboRow(s, 'panel-position', _('Panel position'), '', [
        ['left', _('Left')],
        ['center', _('Center')],
        ['right', _('Right')],
    ]));
    group.add(comboRow(s, 'icon-style', _('Icon style'), '', [
        ['package', _('Package')],
        ['arrow', _('Arrow')],
        ['theme', _('Icon theme')],
    ]));
    page.add(group);
    return page;
}

function upgradePage(s) {
    const page = new Adw.PreferencesPage({title: _('Upgrade'), icon_name: 'software-update-available-symbolic'});

    const term = new Adw.PreferencesGroup({title: _('Terminal emulator')});
    const detected = detectTerminal();
    const choices = [
        ['auto', detected
            ? _('Automatic (%s)').format(TERMINALS[detected].label)
            : _('Automatic (none found)')],
        ...SELECTABLE.map(t => [t, TERMINALS[t].label +
            (GLib.find_program_in_path(t) ? '' : _(' (not installed)'))]),
        ['custom', _('Custom')],
    ];
    term.add(comboRow(s, 'terminal-app', _('Terminal'), _('Used by “Upgrade now…”'), choices));
    const custom = new Adw.EntryRow({title: _('Custom command ({cmd} = bash -c SCRIPT)')});
    custom.add_css_class('monospace');
    s.bind('terminal-custom', custom, 'text', Gio.SettingsBindFlags.DEFAULT);
    const syncCustom = () => {
        custom.sensitive = s.get_string('terminal-app') === 'custom';
    };
    syncCustom();
    const id = s.connect('changed::terminal-app', syncCustom);
    custom.connect('destroy', () => s.disconnect(id));
    term.add(custom);
    page.add(term);

    page.add(stringListGroup(s, 'upgrade-commands', {
        title: _('Upgrade commands'),
        description: _('Run in order in the terminal; a failing command does not stop the next one. Leave empty for the automatic default.'),
        addTitle: _('Add command, e.g. paru -Syu'),
        ordered: true,
        emptyText: () => _('Automatic: %s').format(defaultUpgradeCommands(s).join(' ; ')),
    }));
    return page;
}

function advancedPage(s, window) {
    const page = new Adw.PreferencesPage({title: _('Advanced'), icon_name: 'applications-engineering-symbolic'});

    const daemon = new Adw.PreferencesGroup({title: _('Daemon')});
    const status = new Adw.ActionRow({title: _('Daemon status'), subtitle: _('Querying…')});
    const statusIcon = new Gtk.Image({icon_name: 'content-loading-symbolic'});
    status.add_prefix(statusIcon);
    const mkButton = label => new Gtk.Button({label, valign: Gtk.Align.CENTER});
    const startBtn = mkButton(_('Start'));
    const restartBtn = mkButton(_('Restart'));
    const enableBtn = mkButton(_('Enable at login'));
    for (const b of [enableBtn, startBtn, restartBtn])
        status.add_suffix(b);
    daemon.add(status);

    const last = new Adw.ActionRow({title: _('Last check'), subtitle: '…'});
    const checkBtn = mkButton(_('Check now'));
    last.add_suffix(checkBtn);
    daemon.add(last);

    let busy = false;
    const refresh = async () => {
        const r = await run(['systemctl', '--user', 'show', SERVICE_UNIT,
            '--property=LoadState,ActiveState,SubState,UnitFileState']);
        const props = Object.fromEntries(r.stdout.split('\n')
            .filter(l => l.includes('='))
            .map(l => [l.slice(0, l.indexOf('=')), l.slice(l.indexOf('=') + 1)]));

        if (!r.ok) {
            status.subtitle = _('systemctl failed: %s').format(r.stderr.trim());
            statusIcon.icon_name = 'dialog-error-symbolic';
        } else if (props.LoadState === 'not-found') {
            status.subtitle = _('%s is not installed').format(SERVICE_UNIT);
            statusIcon.icon_name = 'dialog-error-symbolic';
        } else {
            const active = props.ActiveState === 'active';
            status.subtitle = `${props.ActiveState} (${props.SubState}) · ${props.UnitFileState || '?'}`;
            statusIcon.icon_name = active ? 'emblem-default-symbolic' : 'dialog-warning-symbolic';
            startBtn.visible = !active;
            restartBtn.visible = active;
            enableBtn.visible = props.UnitFileState !== 'enabled' && props.UnitFileState !== 'static';
        }
        for (const b of [startBtn, restartBtn, enableBtn])
            b.sensitive = !busy && props.LoadState !== 'not-found';

        const st = await loadStateFile();
        if (!st) {
            last.subtitle = _('No state file yet');
        } else {
            let sub = formatAgo(st.lastCheck, _, ngettext);
            sub += ` · ${ngettext('%d update', '%d updates', st.count).format(st.count)}`;
            if (st.message)
                sub += ` · ${st.message}`;
            last.subtitle = sub;
        }
    };

    const systemctl = async (verb, ...extra) => {
        busy = true;
        await refresh();
        const r = await run(['systemctl', '--user', verb, ...extra, SERVICE_UNIT]);
        busy = false;
        if (!r.ok)
            toast(window, _('systemctl --user %s failed: %s').format(verb, r.stderr.trim()));
        await refresh();
    };
    startBtn.connect('clicked', () => systemctl('start'));
    restartBtn.connect('clicked', () => systemctl('restart'));
    enableBtn.connect('clicked', () => systemctl('enable', '--now'));

    checkBtn.connect('clicked', () => {
        Gio.DBus.session.call(BUS_NAME, OBJECT_PATH, INTERFACE, 'CheckNow', null,
            new GLib.VariantType('(b)'), Gio.DBusCallFlags.NONE, 30000, null, (conn, res) => {
                try {
                    conn.call_finish(res);
                    toast(window, _('Check requested'));
                } catch (e) {
                    toast(window, _('Cannot reach the daemon: %s').format(e.message));
                }
                refresh();
            });
    });

    refresh();
    const timerId = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 5, () => {
        refresh();
        return GLib.SOURCE_CONTINUE;
    });
    window.connect('close-request', () => {
        GLib.source_remove(timerId);
        return false;
    });
    page.add(daemon);

    const logs = new Adw.PreferencesGroup();
    logs.add(comboRow(s, 'log-level', _('Daemon log level'), _('journalctl --user -u arch-update-notifier'), [
        ['error', 'error'],
        ['warn', 'warn'],
        ['info', 'info'],
        ['debug', 'debug'],
        ['trace', 'trace'],
    ]));
    page.add(logs);

    page.add(stringListGroup(s, 'ignore-list', {
        title: _('Ignore list'),
        description: _('Packages excluded from the count, the menu and notifications. Globs * and ? are allowed, e.g. linux* or *-git.'),
        addTitle: _('Add package or pattern'),
    }));
    return page;
}

export default class ArchUpdateNotifierPrefs extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();
        // Keep the settings object alive as long as the window.
        window._settings = settings;
        window.set_default_size(720, 760);
        window.search_enabled = true;

        window.add(checkingPage(settings));
        window.add(notificationsPage(settings));
        window.add(terminalPage(settings, window));
        window.add(indicatorPage(settings));
        window.add(upgradePage(settings));
        window.add(advancedPage(settings, window));
    }
}
