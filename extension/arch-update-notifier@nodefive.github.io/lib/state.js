// Shared constants and state.json helpers. Imported by extension.js and
// prefs.js, so this module must only use GLib (no Shell or GTK imports) and
// must not do any work at import time.

import GLib from 'gi://GLib';

export const BUS_NAME = 'io.github.nodefive.ArchUpdateNotifier';
export const OBJECT_PATH = '/io/github/nodefive/ArchUpdateNotifier';
export const INTERFACE = 'io.github.nodefive.ArchUpdateNotifier1';
export const SERVICE_UNIT = 'arch-update-notifier.service';

// Order and labels of the sources in the menu.
export const SOURCES = [
    {key: 'repo', title: 'Official repositories'},
    {key: 'aur', title: 'AUR'},
    {key: 'flatpak', title: 'Flatpak'},
    {key: 'snap', title: 'Snap'},
];

export function stateDir() {
    return GLib.build_filenamev([GLib.get_user_cache_dir(), 'arch-update-notifier']);
}

export function stateFilePath() {
    return GLib.build_filenamev([stateDir(), 'state.json']);
}

/**
 * Validate and normalise a state.json document. Never throws: a malformed
 * file yields {ok: false, error}.
 *
 * @param {string} text raw file contents
 * @returns {{ok: boolean, state?: object, error?: string}}
 */
export function parseState(text) {
    let raw;
    try {
        raw = JSON.parse(text);
    } catch (e) {
        return {ok: false, error: `state file is not valid JSON (${e.message})`};
    }
    if (raw === null || typeof raw !== 'object' || Array.isArray(raw))
        return {ok: false, error: 'state file has an unexpected format'};

    const num = v => (Number.isFinite(v) ? v : 0);
    const str = v => (typeof v === 'string' ? v : '');
    const pkgs = v => (Array.isArray(v) ? v : [])
        .filter(p => p && typeof p.name === 'string')
        .map(p => ({name: p.name, old: str(p.old), new: str(p.new)}));

    const sources = {};
    const rawSources = raw.sources && typeof raw.sources === 'object' ? raw.sources : {};
    for (const {key} of SOURCES) {
        const s = rawSources[key];
        if (!s || typeof s !== 'object')
            continue;
        sources[key] = {
            error: typeof s.error === 'string' ? s.error : null,
            checkedAt: num(s.checked_at),
            packages: pkgs(s.packages),
            ignored: pkgs(s.ignored),
        };
    }

    const count = Object.values(sources).reduce((n, s) => n + s.packages.length, 0);
    return {
        ok: true,
        state: {
            status: str(raw.status) || 'unknown',
            message: typeof raw.message === 'string' ? raw.message : null,
            lastCheck: num(raw.last_check),
            lastSuccess: num(raw.last_success),
            nextCheck: num(raw.next_check),
            // Recomputed from the lists so the badge and the menu never disagree.
            count,
            ignoredCount: num(raw.ignored_count),
            sources,
        },
    };
}

/**
 * Relative time, e.g. "just now", "12 min ago", "3 h ago", "2 days ago".
 *
 * @param {number} epoch unix seconds
 * @param {Function} _ gettext
 * @param {Function} ngettext ngettext
 * @returns {string}
 */
export function formatAgo(epoch, _, ngettext) {
    if (!epoch)
        return _('never');
    const secs = Math.max(0, Math.floor(GLib.get_real_time() / 1e6) - epoch);
    if (secs < 60)
        return _('just now');
    if (secs < 3600) {
        const m = Math.floor(secs / 60);
        return ngettext('%d min ago', '%d min ago', m).format(m);
    }
    if (secs < 86400) {
        const h = Math.floor(secs / 3600);
        return ngettext('%d hour ago', '%d hours ago', h).format(h);
    }
    const d = Math.floor(secs / 86400);
    return ngettext('%d day ago', '%d days ago', d).format(d);
}
