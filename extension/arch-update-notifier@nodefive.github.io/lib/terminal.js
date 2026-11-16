// Terminal detection and the "Upgrade now" command line. Shared by
// extension.js and prefs.js; GLib only, no work at import time.
//
// The upgrade commands are the user's own configured strings and are run
// interactively in their terminal through bash, which is the point of the
// action. Nothing here runs in the shell process except the spawn itself.

import GLib from 'gi://GLib';

import {BUS_NAME, OBJECT_PATH, INTERFACE} from './state.js';

// How each terminal takes a command (argv after the terminal's own options).
export const TERMINALS = {
    'ptyxis': {label: 'Ptyxis', argv: cmd => ['ptyxis', '--new-window', '--', ...cmd]},
    'kgx': {label: 'Console (kgx)', argv: cmd => ['kgx', '--', ...cmd]},
    'gnome-terminal': {label: 'GNOME Terminal', argv: cmd => ['gnome-terminal', '--', ...cmd]},
    'kitty': {label: 'kitty', argv: cmd => ['kitty', ...cmd]},
    'alacritty': {label: 'Alacritty', argv: cmd => ['alacritty', '-e', ...cmd]},
    // Further fallbacks for auto-detection only.
    'foot': {label: 'foot', argv: cmd => ['foot', ...cmd]},
    'wezterm': {label: 'WezTerm', argv: cmd => ['wezterm', 'start', '--', ...cmd]},
    'konsole': {label: 'Konsole', argv: cmd => ['konsole', '-e', ...cmd]},
    'xdg-terminal-exec': {label: 'xdg-terminal-exec', argv: cmd => ['xdg-terminal-exec', ...cmd]},
    'xterm': {label: 'xterm', argv: cmd => ['xterm', '-e', ...cmd]},
};

// Selectable in preferences (schema choices), besides "auto" and "custom".
export const SELECTABLE = ['ptyxis', 'kgx', 'gnome-terminal', 'kitty', 'alacritty'];

const AUTO_ORDER = ['ptyxis', 'kgx', 'gnome-terminal', 'kitty', 'alacritty',
    'foot', 'wezterm', 'konsole', 'xdg-terminal-exec', 'xterm'];

/** @returns {string|null} first installed terminal in AUTO_ORDER */
export function detectTerminal() {
    return AUTO_ORDER.find(t => GLib.find_program_in_path(t) !== null) ?? null;
}

/**
 * Default upgrade commands when the user configured none.
 *
 * @param {Gio.Settings} settings extension settings
 * @returns {string[]}
 */
export function defaultUpgradeCommands(settings) {
    const cmds = [];
    if (GLib.find_program_in_path('paru'))
        cmds.push('paru -Syu');
    else if (GLib.find_program_in_path('yay'))
        cmds.push('yay -Syu');
    else
        cmds.push('sudo pacman -Syu');
    if (settings.get_boolean('check-flatpak') && GLib.find_program_in_path('flatpak'))
        cmds.push('flatpak update');
    if (settings.get_boolean('check-snap') && GLib.find_program_in_path('snap'))
        cmds.push('sudo snap refresh');
    return cmds;
}

export function upgradeCommands(settings) {
    const configured = settings.get_strv('upgrade-commands').map(c => c.trim()).filter(c => c);
    return configured.length ? configured : defaultUpgradeCommands(settings);
}

/**
 * The bash script run in the terminal: each command in order (continuing on
 * failure), then a CheckNow D-Bus call so the indicator updates, then wait
 * for Enter so the output stays readable.
 *
 * @param {string[]} commands shell command lines
 * @returns {string}
 */
export function buildScript(commands) {
    const q = s => GLib.shell_quote(s);
    const lines = ['rc=0'];
    for (const c of commands) {
        lines.push(`printf '\\n\\033[1m:: %s\\033[0m\\n' ${q(c)}`);
        lines.push(`${c} || rc=$?`);
    }
    lines.push(`gdbus call --session --dest ${q(BUS_NAME)} --object-path ${q(OBJECT_PATH)} ` +
        `--method ${q(`${INTERFACE}.CheckNow`)} >/dev/null 2>&1`);
    lines.push('printf \'\\nFinished (exit status %s). Press Enter to close.\' "$rc"');
    lines.push('read -r _');
    return lines.join('\n');
}

/**
 * Full argv for Gio.Subprocess.
 *
 * @param {Gio.Settings} settings extension settings
 * @returns {string[]}
 * @throws {Error} when no terminal is available or the custom line is invalid
 */
export function buildUpgradeArgv(settings) {
    const bashArgv = ['bash', '-c', buildScript(upgradeCommands(settings))];
    let choice = settings.get_string('terminal-app');

    if (choice === 'custom') {
        const line = settings.get_string('terminal-custom').trim();
        if (!line)
            throw new Error('The custom terminal command is empty');
        const [ok, argv] = GLib.shell_parse_argv(line);
        if (!ok || !argv.length)
            throw new Error('Cannot parse the custom terminal command');
        const i = argv.indexOf('{cmd}');
        if (i >= 0)
            argv.splice(i, 1, ...bashArgv);
        else
            argv.push(...bashArgv);
        return argv;
    }

    if (choice === 'auto' || !TERMINALS[choice] || !GLib.find_program_in_path(choice))
        choice = detectTerminal();
    if (!choice)
        throw new Error('No supported terminal emulator found');
    return TERMINALS[choice].argv(bashArgv);
}
