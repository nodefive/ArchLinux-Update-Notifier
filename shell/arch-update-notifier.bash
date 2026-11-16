# arch-update-notifier: update summary for interactive bash shells.
#
# Source it from ~/.bashrc:
#   [ -r /usr/share/arch-update-notifier/arch-update-notifier.bash ] && . /usr/share/arch-update-notifier/arch-update-notifier.bash
#
# It only reads files written by the daemon. Main path: summary.txt, parsed
# with bash builtins only (no fork at all). Fallback when summary.txt is
# missing (older daemon): one jq call on state.json, if jq is installed.
#
# Environment:
#   AUN_MOTD=0        disable the message for this shell
#   AUN_NO_COLOR=1    plain output
# The message is printed once per shell tree (AUN_MOTD_SHOWN is exported, so
# subshells and nested bash sessions stay quiet).

__aun_motd() {
    [[ $- == *i* ]] || return 0
    [[ ${AUN_MOTD:-1} != 0 && -z ${AUN_MOTD_SHOWN-} ]] || return 0
    export AUN_MOTD_SHOWN=1

    local dir=${XDG_CACHE_HOME:-$HOME/.cache}/arch-update-notifier
    local now=${EPOCHSECONDS:-0}
    local bold='' yellow='' reset=''
    if [[ -t 1 && -z ${AUN_NO_COLOR-} && -z ${NO_COLOR-} ]]; then
        bold=$'\e[1m' yellow=$'\e[33m' reset=$'\e[0m'
    fi

    # "12 min ago" from a unix timestamp, pure bash.
    __aun_ago() {
        local s=$(( now - $1 ))
        if (( $1 <= 0 )); then REPLY='never'
        elif (( s < 60 )); then REPLY='just now'
        elif (( s < 3600 )); then REPLY="$(( s / 60 )) min ago"
        elif (( s < 86400 )); then REPLY="$(( s / 3600 )) h ago"
        else REPLY="$(( s / 86400 )) days ago"
        fi
    }

    if [[ -r $dir/summary.txt ]]; then
        local -a lines
        mapfile -t lines < "$dir/summary.txt" || return 0
        local magic enabled last_check last_success stale
        IFS=$'\t' read -r magic enabled last_check last_success stale <<< "${lines[0]-}"
        [[ $magic == AUN1 && $enabled == 1 ]] || { unset -f __aun_ago; return 0; }
        [[ $last_check =~ ^[0-9]+$ && $last_success =~ ^[0-9]+$ && $stale =~ ^[0-9]+$ ]] || { unset -f __aun_ago; return 0; }

        __aun_ago "$last_check"
        local ago=$REPLY i line
        for (( i = 1; i < ${#lines[@]}; i++ )); do
            line=${lines[i]//\{ago\}/$ago}
            if (( i == 1 )); then
                printf '%s%s%s\n' "$bold" "$line" "$reset"
            else
                printf '%s\n' "$line"
            fi
        done

        # Staleness: last successful check too old, or checks keep failing.
        if (( stale > 0 && now > 0 )); then
            if (( last_success > 0 && now - last_success > stale )); then
                __aun_ago "$last_success"
                printf '%swarning:%s update information is stale (last successful check %s). Is arch-update-notifier.service running?\n' \
                    "$yellow" "$reset" "$REPLY"
            elif (( last_success == 0 && last_check > 0 && now - last_check > stale )); then
                printf '%swarning:%s no update check has succeeded yet. See: journalctl --user -u arch-update-notifier\n' \
                    "$yellow" "$reset"
            fi
        fi
    elif [[ -r $dir/state.json ]] && type -P jq >/dev/null; then
        # Fallback without summary.txt: count and age only.
        local out
        out=$(jq -r '"\(.count // 0)\t\(.last_check // 0)"' "$dir/state.json" 2>/dev/null) || { unset -f __aun_ago; return 0; }
        local count=${out%%$'\t'*} lc=${out#*$'\t'}
        if [[ $count =~ ^[0-9]+$ && $count -gt 0 ]]; then
            __aun_ago "$lc"
            printf '%s%s update(s) available%s — last checked %s\n' "$bold" "$count" "$reset" "$REPLY"
        fi
    fi
    unset -f __aun_ago
}

__aun_motd
unset -f __aun_motd
