#!/usr/bin/env bash
# Reads the alpha testers' diagnostics from the live server (diagnostics/,
# docs/DIAGNOSTICS_CONTRACT.md), for any agent or person debugging their
# problems:
#
#   tools/diagnostics.sh stats
#       installs, most recent first: account, app version, OS, first and
#       last seen, records and reports sent
#   tools/diagnostics.sh export <out.jsonl> [--install <id>] [--account <id>]
#                               [--since <date>] [--until <date>] [--kind <kind>]
#       the records, one per line as the app sent them, ordered by time
#       across installs (both ends of a share read as one story). --since
#       keeps what is at or after it, --until what is before it; a date is
#       2026-11-19 or an instant, 2026-11-19T09:00:00Z. --kind is system,
#       log, stores, panic, previous_run or state.
#   tools/diagnostics.sh reports
#       the "Send Diagnostics..." reports, most recent first: code, install,
#       account, app, when, the note
#   tools/diagnostics.sh report <code> <out.json>
#       one report whole: its note, the stores' shape, the log files' text
#
# Uses the read key in ~/.config/pimble-release/diagnostics-read-key
# (PIMBLE_DIAGNOSTICS_READ_KEY_FILE to change it), which only reads: the
# builds' send key cannot. PIMBLE_DIAGNOSTICS_URL points at another server
# (diagnostics/dev.sh's is http://127.0.0.1:8788, read key dev-read-key).
# Exports name stores and paths: keep them out of the repo.
set -euo pipefail

base="${PIMBLE_DIAGNOSTICS_URL:-https://pimble-diagnostics.jkbase.app}"
base="${base%/}"
key_file="${PIMBLE_DIAGNOSTICS_READ_KEY_FILE:-$HOME/.config/pimble-release/diagnostics-read-key}"

usage() { sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }

[ $# -gt 0 ] || usage
[ -f "$key_file" ] || { echo "no read key at $key_file (see diagnostics/README.md)" >&2; exit 1; }

# The key goes to curl on stdin, never on a command line.
get() {
    printf 'header = "Authorization: Bearer %s"\n' "$(tr -d '[:space:]' < "$key_file")" |
        curl --config - --silent --show-error --fail-with-body --compressed "$@"
}

# A failed download holds the server's answer, not data: say it, and drop it.
failed() { cat "$1" >&2; echo >&2; rm -f "$1"; exit 1; }

case "$1" in
    stats)
        [ $# -eq 1 ] || usage
        get "$base/v1/stats"
        echo
        ;;
    export)
        out="${2:-}"; [ -n "$out" ] || usage
        shift 2
        params=()
        while [ $# -gt 0 ]; do
            case "$1" in
                --install|--account|--since|--until|--kind)
                    [ $# -ge 2 ] || { echo "$1 needs a value" >&2; exit 2; }
                    params+=(--data-urlencode "${1#--}=$2"); shift 2 ;;
                *) usage ;;
            esac
        done
        get --get "$base/v1/export" "${params[@]}" -o "$out" || failed "$out"
        echo "$(wc -l < "$out") records → $out" >&2
        ;;
    reports)
        [ $# -eq 1 ] || usage
        get "$base/v1/reports"
        echo
        ;;
    report)
        code="${2:-}" out="${3:-}"
        [[ "$code" =~ ^[0-9A-Za-z-]{8,9}$ ]] && [ -n "$out" ] || usage
        get "$base/v1/reports/$code" -o "$out" || failed "$out"
        echo "$out" >&2
        ;;
    *) usage ;;
esac
