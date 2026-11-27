#!/bin/sh
# Held: schedule a host group's next macOS maintenance window: one run of the kind `macos` on the
# group, with a window start and end, which takes each Mac with a minor update due in turn, by the
# group's policy (batch, min_serving, drain deadline; mac-policy.sh sets them).
#
# Run by the owner only, from an operator box whose `hk` has its settings file
# (`hk settings init`, docs/cli.md), with `jq` on PATH. Nothing else runs it.
#
# A run's window is the only schedule the coordinator holds: there is no standing, repeating one
# (komira-ai/hostkeeper#603). So the standing schedule is this script, run once for each window;
# the window chosen for mac-build is in docs/operations/maintenance-first-mac-run.md, "The
# schedule". Run it after macos-update.sh has updated one Mac.
#
# Without --apply (the default) it changes nothing: it reads the group and the open maintenance
# runs, asks the coordinator's Schedule preview (a `maintenance schedule` without `--confirm`,
# which stores nothing and writes no audit entry) and prints, for each Mac, whether the run would
# take it (waiting) or leave macOS out (skipped, and why), and prints the one command --apply
# would run as `DRY-RUN:`.
#
# With --apply: the same reads and checks, then that one `hk maintenance schedule ... --confirm
# true`, and the run's id and the line that cancels it. It does not follow the run: it starts at
# the window. It never passes --take-last-host, so a run that would take every Mac of the group is
# refused last_host (once a new update is due on all of them): give --except with one Mac, and
# the run names the group's other Macs by --host-ids in place of the group. That Mac is taken by
# the next window, or by macos-update.sh.
#
# It refuses (exit 1) while a run that is not ended names the group or one of its hosts, while the
# group's macos_major is true (a turn would take a new major), when the window starts in the past,
# and when the preview takes no Mac.
#
# Every read and change is printed before it runs: `READ:` a read, `+` a change. With --record
# DIR (an existing directory, the workspace scratch directory and not /tmp) each answer is also
# written there; a file already there is never overwritten.
#
# Exit: 0 the run is shown (dry run) or stored; 1 a refusal or a check that failed; 2 a usage error.

set -eu

APPLY=0
GROUP=mac-build
START=""
HOURS=6
RECORD=""
REASON=""
EXCEPT=""

usage() {
    cat <<'EOF'
usage: sh mac-schedule.sh --start TIME [--hours N] [--group NAME] [--except ID]... [--apply]
                          [--record DIR] [--reason TEXT]

  --start TIME   when the window opens, in UTC, as 2026-12-05T22:00:00Z
  --hours N      how long it stays open, 1 to 24 (default 6); a Mac in its turn at the end
                 finishes it
  --group NAME   the host group (default mac-build)
  --except ID    leave this Mac of the group out, repeatable; the run then names the others by
                 --host-ids, so it does not take the group's last host
  --apply        store the run; without it nothing is changed
  --record DIR   write each answer into DIR (it must exist)
  --reason TEXT  the run's reason (default: "macOS window of <group>, <start>")
EOF
}

die_usage() {
    printf 'mac-schedule: %s\n' "$1" >&2
    usage >&2
    exit 2
}

while [ $# -gt 0 ]; do
    case $1 in
        --apply) APPLY=1 ;;
        --start | --hours | --group | --except | --record | --reason)
            [ $# -ge 2 ] || die_usage "$1 needs a value"
            case $1 in
                --start) START=$2 ;;
                --hours) HOURS=$2 ;;
                --group) GROUP=$2 ;;
                --except) EXCEPT="$EXCEPT $2" ;;
                --record) RECORD=$2 ;;
                --reason) REASON=$2 ;;
            esac
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) die_usage "unknown argument: $1" ;;
    esac
    shift
done

case $GROUP in
    '' | -* | *[!a-z0-9-]*) die_usage "--group is a group name of [a-z0-9-]: $GROUP" ;;
esac
case $HOURS in
    '' | *[!0-9]* | 0*) die_usage "--hours is 1 to 24: $HOURS" ;;
esac
for mac in $EXCEPT; do
    case $mac in
        -* | *[!a-z0-9-]*) die_usage "--except is a node id of [a-z0-9-]: $mac" ;;
    esac
done
[ "$HOURS" -le 24 ] || die_usage "--hours is 1 to 24: $HOURS"
[ -n "$START" ] || die_usage "--start is needed"
printf '%s' "$START" | grep -Eqx '[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z' ||
    die_usage "--start is a UTC time as 2026-12-05T22:00:00Z: $START"
case $REASON in
    ' '* | *' ' | "$(printf '\t')"* | *"$(printf '\t')") die_usage "--reason has a space at an end, which the coordinator trims from its audit entry" ;;
esac
if [ -n "$RECORD" ] && [ ! -d "$RECORD" ]; then
    die_usage "--record names no directory: $RECORD"
fi
for tool in hk jq grep awk; do
    command -v "$tool" >/dev/null 2>&1 || die_usage "$tool is not on PATH"
done
# jq reads the time, so the script is the same on a Linux box and a Mac. A time that does not
# exist (2026-11-31) is refused by printing it back and comparing.
FROM=$(jq -nr --arg s "$START" '$s | fromdateiso8601? // empty | floor') || FROM=""
[ -n "$FROM" ] && [ "$(jq -nr --argjson t "$FROM" '$t | todateiso8601')" = "$START" ] ||
    die_usage "--start is not a time: $START"
UNTIL=$((FROM + HOURS * 3600))
NOW=$(jq -n 'now | floor')
[ "$FROM" -gt "$NOW" ] || die_usage "--start is not in the future: $START"
[ -n "$REASON" ] || REASON="macOS window of $GROUP, $START"

say() { printf '%s\n' "$*"; }

read_hk() {
    say "READ: hk $*" >&2
    hk "$@"
}

change_hk() {
    say "+ hk $*" >&2
    hk "$@"
}

record() {
    [ -n "$RECORD" ] || return 0
    if [ -e "$RECORD/$1" ]; then
        say "mac-schedule: $RECORD/$1 is there already; not written" >&2
        return 0
    fi
    printf '%s\n' "$2" >"$RECORD/$1"
}

grp=$(read_hk host-groups get --host-group "$GROUP") || { say "the group could not be read"; exit 1; }
record "$GROUP-group.json" "$grp"
if printf '%s' "$grp" | jq -e '(.nextPageToken // "") != ""' >/dev/null; then
    say "the group has more hosts than one page; the run check below would miss some"
    exit 1
fi
NODES=$(printf '%s' "$grp" | jq -c '.nodeIds // []')
POLICY=$(printf '%s' "$grp" | jq -c '.hostGroup.group.policy.maintenance // {}')
say "group $GROUP: hosts $(printf '%s' "$NODES" | jq -r 'join(" ")')"
say "  maintenance: $POLICY"
say "window: $START to $(jq -nr --argjson t "$UNTIL" '$t | todateiso8601') ($FROM to $UNTIL)"
if [ "$(printf '%s' "$POLICY" | jq -r '.macosMajor // false')" = true ]; then
    say "refused: the group's macos_major is true, so a turn would take a new major; mac-policy.sh sets it off"
    exit 1
fi

runs=$(read_hk maintenance list --states scheduled --states running --states halted) ||
    { say "the maintenance runs could not be read"; exit 1; }
open=$(printf '%s' "$runs" | jq -r --arg g "$GROUP" --argjson nodes "$NODES" '.runs[]?
    | select(.hostGroup == $g or ((.hostIds // []) | any(. as $h | $nodes | index($h))))
    | "\(.id) \(.state) \((.kinds // []) | map(sub("MAINTENANCE_KIND_"; "") | ascii_downcase) | join(","))"')
if [ -n "$open" ]; then
    say "refused: a maintenance run is open on the group or its hosts; let it end or cancel it first:"
    say "$open"
    exit 1
fi

if [ -z "$EXCEPT" ]; then
    set -- maintenance schedule --host-group "$GROUP" --kinds macos
else
    EX=$(printf '%s\n' $EXCEPT | jq -Rsc 'split("\n") | map(select(. != ""))')
    missing=$(jq -nr --argjson e "$EX" --argjson n "$NODES" '$e - $n | join(" ")')
    [ -z "$missing" ] || { say "refused: --except names no host of $GROUP: $missing"; exit 1; }
    TAKE=$(jq -nr --argjson e "$EX" --argjson n "$NODES" '$n - $e | join(" ")')
    [ -n "$TAKE" ] || { say "refused: --except leaves no Mac of $GROUP"; exit 1; }
    [ "$(printf '%s\n' $TAKE | awk 'END { print NR }')" -le 32 ] || { say "refused: a run names at most 32 hosts"; exit 1; }
    say "left out: $(printf '%s' "$EX" | jq -r 'join(" ")'); the run names $TAKE"
    set -- maintenance schedule --kinds macos
    for mac in $TAKE; do
        set -- "$@" --host-ids "$mac"
    done
fi
set -- "$@" --window-start-unix "$FROM" --window-end-unix "$UNTIL"
preview=$(read_hk "$@") || { say "the coordinator refused the preview (its reason is above); nothing is stored"; exit 1; }
record "$GROUP-preview.json" "$preview"
printf '%s' "$preview" | jq -r '.hosts[]? | "  \(.hostId): \(.step // "-" | sub("MAINTENANCE_HOST_STEP_"; "") | ascii_downcase)\(if .skippedBecause then " " + (.skippedBecause | sub("MAINTENANCE_SKIP_"; "") | ascii_downcase) else "" end)"'
waiting=$(printf '%s' "$preview" | jq '[.hosts[]? | select(.step == "MAINTENANCE_HOST_STEP_WAITING")] | length')
if [ "$waiting" -eq 0 ]; then
    say "refused: the run would take no Mac"
    exit 1
fi
say "the run would take $waiting Mac(s), $(printf '%s' "$POLICY" | jq -r '"batch \(.batch // 1), min_serving \(.minServing // 1)"')"

if [ "$APPLY" -eq 0 ]; then
    say "DRY-RUN: hk $* --confirm true --reason \"$REASON\""
    exit 0
fi

resp=$(change_hk "$@" --confirm true --reason "$REASON") ||
    { say "the schedule was refused (its reason is above); nothing is stored"; exit 1; }
record "$GROUP-schedule.json" "$resp"
run=$(printf '%s' "$resp" | jq -r '.run.id // empty')
[ -n "$run" ] || { say "the answer names no run: $resp"; exit 1; }
say "audit $(printf '%s' "$resp" | jq -r '.entry | "\(.seq) \(.action) by \(.byName // .by) group \(.hostGroup // "-"): \(.reason)"')"
say "run $run scheduled, $(printf '%s' "$resp" | jq -r '.run.state // "-" | sub("MAINTENANCE_STATE_"; "") | ascii_downcase')"
say "to follow it: hk maintenance get --run-id $run"
say "to cancel it: hk maintenance cancel --run-id $run --reason \"<why>\""
exit 0
