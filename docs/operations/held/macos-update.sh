#!/bin/sh
# Held: install the newest minor macOS update on one farm Mac through one hostkeeper maintenance
# run of the kind `macos` (stage, drain, apply, restart, check, put back), then prove the Mac back
# with one run of the kind `probe` (drain, read sw_vers and FileVault as root, put back).
#
# Run by the owner only, from an operator box whose `hk` has its settings file
# (`hk settings init`, docs/cli.md), with `jq` and `awk` on PATH. Nothing else runs it.
#
# Without --apply (the default) it changes nothing: it reads the Mac (`hk hosts get`), its group
# (`hk host-groups get`), the open runs and the coordinator's Schedule preview (a
# `maintenance schedule` without `--confirm`, which stores nothing and writes no audit entry),
# says whether the Mac is ready, and prints the two commands --apply would run as `DRY-RUN:`.
#
# A Mac is ready when the coordinator would order it macos (no device management conflict, the
# update account present, admin and with a secure token, FileVault off: no run orders macos on a
# Mac whose FileVault is not off, so the authenticated restart is never reached), a macOS update
# of its running major is due, its report is fresh, its daemon is at least MIN_DAEMON and takes a
# probe, its group's macos_major is off (a turn would otherwise take a new major), its group has
# another host (the script never passes --take-last-host), and no run that is not ended names it.
#
# With --apply: the same checks again, then
#   1. `hk maintenance schedule --kinds macos --host-ids <mac> --confirm true`, then
#      `hk maintenance get` every --poll-secs seconds until the Mac's turn ends. A Mac not_back
#      (silent past its group's return_secs after the restart) is followed on: its turn goes on
#      when it reports again. The turn ends done only when the Mac then reports a macOS build
#      other than the one it ran and a version at least the one due; `applied` alone is not
#      proof, since a put back also ends a turn done.
#   2. `hk maintenance schedule --kinds probe --host-ids <mac> --confirm true`, followed the same
#      way, then the Mac's report read until it holds that run's probe, whose sw_vers is printed.
#   3. The audit entries of both runs, and the Mac's sw_vers before (its report) and after (its
#      report and the probe).
# It stops at the first step that does not end well:
#   deferred      the Mac is back in service and unchanged; the run is cancelled, so it is not
#                 offered again unattended;
#   halted        the Mac stays drained (cordoned); nothing is done for it: the owner chooses
#                 `maintenance retry-host` or `maintenance put-back`, which are printed;
#   not_reached,  the run took no turn on the Mac; nothing to undo;
#   skipped, or
#   the run ended
#   --max-wait-secs passed  the run goes on; the commands to read and cancel it are printed.
# It never passes --take-last-host, and never retries a refused call.
#
# Every read and change is printed before it runs: `READ:` a read, `+` a change; a run's reads
# are printed once, before the first. With --record DIR (an existing directory, the workspace
# scratch directory and not /tmp), each answer is also written there as <mac>-<what>.json, and the
# audit lines and sw_vers as <mac>-audit.txt and <mac>-sw_vers.txt; a file already there is never
# overwritten.
#
# Exit: 0 the Mac is ready (dry run), or updated and probed (--apply); 1 not ready, or a step
# that did not end well; 2 a usage error.

set -eu

MIN_DAEMON=0.1.480
APPLY=0
MAC=""
RECORD=""
POLL=30
MAX_WAIT=36000
DRAIN=""
LOOK_TRIES=20
READ_TRIES=40

usage() {
    cat <<'EOF'
usage: sh macos-update.sh --mac ID [--apply] [--record DIR] [--poll-secs N]
                          [--max-wait-secs N] [--drain-deadline-secs N]

  --mac ID                 the Mac, by node id
  --apply                  schedule the two runs; without it nothing is changed
  --record DIR             write each answer, the audit lines and sw_vers into DIR (it must exist)
  --poll-secs N            seconds between reads of a run (default 30)
  --max-wait-secs N        stop following a run after this long (default 36000: the 2 h download
                           deadline, the 4 h drain deadline of mac-policy.sh, the 3 h install
                           deadline and the hour a Mac may take to come back)
  --drain-deadline-secs N  the runs' drain deadline (default: the group's)
EOF
}

die_usage() {
    printf 'macos-update: %s\n' "$1" >&2
    usage >&2
    exit 2
}

is_count() {
    case $1 in
        '' | *[!0-9]* | 0*) return 1 ;;
        *) [ "${#1}" -le 9 ] ;;
    esac
}

while [ $# -gt 0 ]; do
    case $1 in
        --apply) APPLY=1 ;;
        --mac | --record | --poll-secs | --max-wait-secs | --drain-deadline-secs)
            [ $# -ge 2 ] || die_usage "$1 needs a value"
            case $1 in
                --mac)
                    [ -z "$MAC" ] || die_usage "--mac is given once: this script updates one Mac"
                    MAC=$2
                    ;;
                --record) RECORD=$2 ;;
                --poll-secs) POLL=$2 ;;
                --max-wait-secs) MAX_WAIT=$2 ;;
                --drain-deadline-secs) DRAIN=$2 ;;
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

[ -n "$MAC" ] || die_usage "--mac is needed"
case $MAC in
    -* | *[!a-z0-9-]*) die_usage "--mac is a node id of [a-z0-9-]: $MAC" ;;
esac
is_count "$POLL" || die_usage "--poll-secs is a whole number above 0: $POLL"
is_count "$MAX_WAIT" || die_usage "--max-wait-secs is a whole number above 0: $MAX_WAIT"
[ -z "$DRAIN" ] || is_count "$DRAIN" || die_usage "--drain-deadline-secs is a whole number above 0: $DRAIN"
if [ -n "$RECORD" ] && [ ! -d "$RECORD" ]; then
    die_usage "--record names no directory: $RECORD"
fi
for tool in hk jq awk sed tr; do
    command -v "$tool" >/dev/null 2>&1 || die_usage "$tool is not on PATH"
done

say() { printf '%s\n' "$*"; }

# READ: a call that changes nothing.
read_hk() {
    say "READ: hk $*" >&2
    hk "$@"
}

# +: a call that changes something; only with --apply.
change_hk() {
    say "+ hk $*" >&2
    hk "$@"
}

# record <name> <text>: keeps <text> as $RECORD/<name>, never over a file already there.
record() {
    [ -n "$RECORD" ] || return 0
    if [ -e "$RECORD/$1" ]; then
        say "macos-update: $RECORD/$1 is there already; not written" >&2
        return 0
    fi
    printf '%s\n' "$2" >"$RECORD/$1"
}

# version_ge A B: A (a leading v allowed) is at least B, compared number by number.
version_ge() {
    awk -v a="${1#v}" -v b="$2" 'BEGIN {
        n = split(a, x, "."); m = split(b, y, "."); k = n > m ? n : m
        for (i = 1; i <= k; i++) { p = x[i] + 0; q = y[i] + 0; if (p > q) exit 0; if (p < q) exit 1 }
        exit 0 }'
}

lower() { printf '%s' "$1" | sed "s/^$2//" | tr 'A-Z' 'a-z'; }

# The facts a macOS turn depends on, one line each.
FACTS='.host as $h | ($h.maintenance // {}) as $m | ($m.mac // {}) as $x | ($x.updateAccount // {}) as $a |
    "  group \($h.hostGroup // "-"), live \(($h | .live) // false), daemon \($h.heartbeat.agent.version // "-")",
    "  macOS \($m.host.osVersion // "-") (\($m.host.osBuild // "-")), free \($x.dataFreeBytes // "-") bytes",
    "  update account: present \($a.present // false), admin \($a.admin // false), secure token \($a.secureToken // false)",
    "  FileVault \(($x.filevault // "FILE_VAULT_STATE_UNSPECIFIED") | sub("FILE_VAULT_STATE_"; "") | ascii_downcase), authrestart supported \($x.authrestartSupported // false)",
    "  device management conflicts: \(($x.mdmConflicts // []) | if . == [] then "none" else join(", ") end)",
    "  look age \($h.maintenanceLookAgeSecs // "-") s, stale \($h.maintenanceStale // false), look failed \(($m.lookFailed // "") | if . == "" then "no" else . end)",
    (if [($m.due // [])[] | select(.kind == "MAINTENANCE_KIND_MACOS")] == [] then "  due macOS: nothing" else
        (($m.due // [])[] | select(.kind == "MAINTENANCE_KIND_MACOS") | "  due macOS \(.from // "-") -> \(.to // "-") \(.label // "")\(if ((.from // "") | split(".")[0]) != ((.to // "") | split(".")[0]) then ", a new major: not taken" else "" end)") end),
    (if ($m | .run) then "  in run \(($m | .run).runId) at \(($m | .run).step)" else empty end)'

# What stops a macOS turn, by the coordinator's rules in their order; none: ready. A due item
# counts only within the running major (from and to share their first number): the group's
# macos_major is off, so the coordinator counts no other (maintenance_mac.rs, counts_as_due).
GAPS='def minor: ((.from // "") | split(".")[0]) == ((.to // "") | split(".")[0]); .host as $h | ($h.maintenance // null) as $m |
    (if (($h | .live) // false) | not then "not live" else empty end),
    (if $m == null then "no maintenance report" else
        ((if ($h.maintenanceStale // false) then "its report is stale" else empty end),
         (if ($m.lookFailed // "") != "" then "its last look failed: \($m.lookFailed)" else empty end),
         (if $m.mac == null then "no_update_account: its report holds no Mac facts" else
            ($m.mac as $x | ($x.updateAccount // {}) as $a |
             (if ($x.mdmConflicts // []) != [] then "mdm_conflict: \($x.mdmConflicts | join(", "))" else empty end),
             (if (($a.present // false) and ($a.admin // false) and ($a.secureToken // false)) | not then "no_update_account" else empty end),
             (if ($x.filevault // "") != "FILE_VAULT_STATE_OFF" then "filevault_not_ready: FileVault is not off" else empty end)) end),
         (if [($m.due // [])[] | select(.kind == "MAINTENANCE_KIND_MACOS" and minor)] == [] then "no macOS update of the running major due" else empty end),
         (if (($m.onDemand // []) | index("MAINTENANCE_KIND_PROBE")) then empty else "its daemon takes no probe" end))
    end)'

# The update the turn installs: the newest due of the running major, by version. Its build is
# what follows the label's last `-` (the daemon's rule, macos/apply.rs).
TARGET='def minor: ((.from // "") | split(".")[0]) == ((.to // "") | split(".")[0]); [(.host.maintenance.due // [])[] | select(.kind == "MAINTENANCE_KIND_MACOS" and minor)]
    | sort_by(.to // "" | split(".") | map(tonumber? // 0)) | last // {}
    | "\(.to // "")\t\(.label // "")\t\((.label // "") | capture("-(?<b>[A-Za-z0-9]+)$").b // "")"'

ready() {
    host=$(read_hk hosts get --node-id "$MAC") || { say "$MAC: not ready: hosts get refused"; return 1; }
    record "$MAC-host-$1.json" "$host"
    say "$MAC:"
    printf '%s' "$host" | jq -r "$FACTS"
    gaps=$(printf '%s' "$host" | jq -r "$GAPS")
    daemon=$(printf '%s' "$host" | jq -r '.host.heartbeat.agent.version // ""')
    if [ -z "$daemon" ] || ! version_ge "$daemon" "$MIN_DAEMON"; then
        gaps="$gaps
daemon ${daemon:-unknown} is older than $MIN_DAEMON"
    fi
    if [ -n "$(printf '%s' "$gaps" | awk 'NF')" ]; then
        say "$MAC: not ready: $(printf '%s\n' "$gaps" | awk 'NF' | awk 'BEGIN { ORS = "" } { if (NR > 1) print "; "; print }')"
        return 1
    fi
    target=$(printf '%s' "$host" | jq -r "$TARGET")
    TO=$(printf '%s' "$target" | awk -F '\t' '{ print $1 }')
    LABEL=$(printf '%s' "$target" | awk -F '\t' '{ print $2 }')
    BUILD=$(printf '%s' "$target" | awk -F '\t' '{ print $3 }')
    WAS_VERSION=$(printf '%s' "$host" | jq -r '.host.maintenance.host.osVersion // ""')
    WAS_BUILD=$(printf '%s' "$host" | jq -r '.host.maintenance.host.osBuild // ""')
    if [ -z "$TO" ] || [ -z "$BUILD" ] || [ -z "$WAS_BUILD" ]; then
        say "$MAC: not ready: the due update or the running build is unknown: to ${TO:--}, label ${LABEL:--}, running ${WAS_BUILD:--}"
        return 1
    fi
    group=$(printf '%s' "$host" | jq -r '.host.hostGroup // ""')
    grp=$(read_hk host-groups get --host-group "$group") || { say "$MAC: not ready: its group ${group:-(none)} could not be read"; return 1; }
    record "$MAC-group-$1.json" "$grp"
    say "  group $group: hosts $(printf '%s' "$grp" | jq -r '(.nodeIds // []) | join(" ")'), maintenance $(printf '%s' "$grp" | jq -c '.hostGroup.group.policy.maintenance // {}')"
    if [ "$(printf '%s' "$grp" | jq -r '.hostGroup.group.policy.maintenance.macosMajor // false')" = true ]; then
        say "$MAC: not ready: its group's macos_major is true, so the turn would take a new major; mac-policy.sh sets it off"
        return 1
    fi
    if [ "$(printf '%s' "$grp" | jq '(.nodeIds // []) | length')" -lt 2 ]; then
        say "$MAC: not ready: it is the only host of $group, and this script never takes a group's last host"
        return 1
    fi
    runs=$(read_hk maintenance list --states scheduled --states running --states halted) ||
        { say "$MAC: not ready: the maintenance runs could not be read"; return 1; }
    open=$(printf '%s' "$runs" | jq -r --arg m "$MAC" --arg g "$group" '.runs[]?
        | select(.hostGroup == $g or ((.hostIds // []) | index($m))) | "\(.id) \(.state)"')
    if [ -n "$open" ]; then
        say "$MAC: not ready: a run that is not ended names it or its group: $open"
        return 1
    fi
    preview=$(read_hk maintenance schedule --kinds macos --host-ids "$MAC") ||
        { say "$MAC: not ready: the coordinator refused the preview (its reason is above)"; return 1; }
    record "$MAC-preview-$1.json" "$preview"
    row=$(printf '%s' "$preview" | jq -c --arg m "$MAC" '[.hosts[]? | select(.hostId == $m)][0] // {}')
    if ! printf '%s' "$row" | jq -e \
        '.step == "MAINTENANCE_HOST_STEP_WAITING" and .kinds == ["MAINTENANCE_KIND_MACOS"]' >/dev/null; then
        say "$MAC: not ready: the preview takes no macos turn on it: $(printf '%s' "$row" | jq -r '"\(.step // "-") \(.skippedBecause // "")"')"
        return 1
    fi
    say "$MAC: ready: macOS $WAS_VERSION ($WAS_BUILD) -> $TO ($BUILD), \"$LABEL\""
    return 0
}

AUDIT=""

# audit <run id> <seq of the schedule entry> <its line>: the run's audit entries, oldest first.
audit() {
    entries=$(read_hk audit list --node-id "$MAC" --since-seq "$2") || { say "$MAC: the audit log could not be read"; return 1; }
    lines=$(printf '%s' "$entries" | jq -r --arg t "run $1" '.entries[]?
        | (.reason // "") as $r
        | select($r == $t or ($r | startswith($t + " ")) or ($r | endswith(" " + $t)))
        | "audit \(.seq) \(.atUnix // "-") \(.action) by \(.byName // .by) node \(.nodeId // "-"): \($r)"')
    AUDIT="$AUDIT$3
$lines
"
    say "$3"
    say "$lines"
}

# follow <run id> <what>: reads the run until the Mac's turn ends; sets STEP, STATE, ROW, GOT.
# 0 the turn ended, 1 it was not followed to its end.
follow() {
    say "READ: hk maintenance get --run-id $1, every $POLL s until the turn ends" >&2
    last=""
    STEP=-
    STATE=-
    failed=0
    waited=0
    while :; do
        if GOT=$(hk maintenance get --run-id "$1"); then
            failed=0
            STEP=$(printf '%s' "$GOT" | jq -r --arg m "$MAC" '[.hosts[]? | select(.hostId == $m)][0].step // "-"')
            STATE=$(printf '%s' "$GOT" | jq -r '.run.state // "-"')
            if [ "$STEP" != "$last" ]; then
                say "$MAC: $2 $(lower "$STEP" MAINTENANCE_HOST_STEP_) (run $(lower "$STATE" MAINTENANCE_STATE_))"
                last=$STEP
            fi
            case $STEP in
                *_DONE | *_DEFERRED | *_HALTED | *_NOT_REACHED | *_SKIPPED) break ;;
                # A Mac not back has its turn go on when it reports again; the run shows halted
                # meanwhile, which is not the end of this turn.
                *_NOT_BACK | *_STAGING | *_DRAINING | *_APPLYING | *_REBOOTING | *_CHECKING | *_PUTTING_BACK) ;;
                *)
                    case $STATE in
                        *_DONE | *_CANCELLED | *_WINDOW_ENDED) break ;;
                    esac
                    ;;
            esac
        else
            failed=$((failed + 1))
            say "$MAC: run $1 could not be read ($failed in a row); it goes on, and is read again"
            [ "$failed" -lt "$READ_TRIES" ] || { stop_following "$1"; return 1; }
        fi
        waited=$((waited + POLL))
        if [ "$waited" -ge "$MAX_WAIT" ]; then
            say "$MAC: $2 not ended after $MAX_WAIT s of reads, at $(lower "$STEP" MAINTENANCE_HOST_STEP_)"
            stop_following "$1"
            return 1
        fi
        sleep "$POLL"
    done
    ROW=$(printf '%s' "$GOT" | jq -c --arg m "$MAC" '[.hosts[]? | select(.hostId == $m)][0] // {}')
    return 0
}

stop_following() {
    say "$MAC: stopped following run $1; the run goes on. Read it with"
    say "  hk maintenance get --run-id $1"
    say "and, once the Mac's turn is halted, retry-host or put-back it as below; or cancel it:"
    say "  hk maintenance cancel --run-id $1 --reason \"<why>\""
}

# ended_badly <run id> <what> <seq> <first audit line>: says why the turn did not end done, and
# acts only on a deferral (cancels the run).
ended_badly() {
    why=$(printf '%s' "$ROW" | jq -r '.reason // .skippedBecause // ""')
    case $STEP in
        *_DEFERRED)
            say "$MAC: $2 deferred: $why; the Mac is back in service, unchanged"
            change_hk maintenance cancel --run-id "$1" \
                --reason "$2 on $MAC deferred; stopped here" >/dev/null ||
                say "$MAC: the cancel was refused; cancel run $1 by hand"
            ;;
        *_HALTED)
            say "$MAC: $2 halted: $why"
            say "$MAC: it stays drained. After reading its log, either:"
            say "  hk maintenance retry-host --run-id $1 --host-id $MAC --reason \"<why>\""
            say "  hk maintenance put-back --run-id $1 --host-id $MAC --reason \"<why>\""
            ;;
        *) say "$MAC: $2 ended $(lower "$STEP" MAINTENANCE_HOST_STEP_) (run $(lower "$STATE" MAINTENANCE_STATE_)) $why" ;;
    esac
    audit "$1" "$3" "$4" || true
}

# schedule <kind> <reason>: stores a run of <kind> on the Mac; sets RUN, SEQ, FIRST.
schedule() {
    set -- maintenance schedule --kinds "$1" --host-ids "$MAC" ${DRAIN:+--drain-deadline-secs "$DRAIN"} \
        --confirm true --reason "$2"
    resp=$(change_hk "$@") || { say "$MAC: the schedule was refused (its reason is above); nothing changed"; return 1; }
    RUN=$(printf '%s' "$resp" | jq -r '.run.id // empty')
    SEQ=$(printf '%s' "$resp" | jq -r '.entry.seq // "0"')
    FIRST=$(printf '%s' "$resp" | jq -r '.entry | "audit \(.seq) \(.atUnix // "-") \(.action) by \(.byName // .by) node -: \(.reason)"')
    [ -n "$RUN" ] || { say "$MAC: the answer names no run: $resp"; return 1; }
    record "$MAC-schedule-$RUN.json" "$resp"
    say "$MAC: run $RUN scheduled"
}

MODE=dry
[ "$APPLY" -eq 0 ] || MODE=apply
ready "$MODE" || exit 1
drain=""
[ -z "$DRAIN" ] || drain=" --drain-deadline-secs $DRAIN"
UPDATE_REASON="macOS $TO ($BUILD) on $MAC"
PROBE_REASON="Probe $MAC after macOS $TO"
if [ "$APPLY" -eq 0 ]; then
    say "DRY-RUN: hk maintenance schedule --kinds macos --host-ids $MAC$drain --confirm true --reason \"$UPDATE_REASON\""
    say "DRY-RUN: hk maintenance schedule --kinds probe --host-ids $MAC$drain --confirm true --reason \"$PROBE_REASON\""
    exit 0
fi
BEFORE="sw_vers before (the Mac's report): $WAS_VERSION ($WAS_BUILD)"
say "$MAC: $BEFORE"

# 1. The update.
schedule macos "$UPDATE_REASON" || exit 1
URUN=$RUN
USEQ=$SEQ
UFIRST=$FIRST
follow "$URUN" macos || exit 1
record "$MAC-run-$URUN.json" "$GOT"
case $STEP in
    *_DONE) ;;
    *)
        ended_badly "$URUN" macos "$USEQ" "$UFIRST"
        exit 1
        ;;
esac
# The daemon looks again when the turn ends; read until the report shows a new build.
tries=0
while :; do
    if host=$(read_hk hosts get --node-id "$MAC"); then
        NOW_VERSION=$(printf '%s' "$host" | jq -r '.host.maintenance.host.osVersion // ""')
        NOW_BUILD=$(printf '%s' "$host" | jq -r '.host.maintenance.host.osBuild // ""')
        if [ -n "$NOW_BUILD" ] && [ "$NOW_BUILD" != "$WAS_BUILD" ]; then
            break
        fi
    fi
    tries=$((tries + 1))
    if [ "$tries" -ge "$LOOK_TRIES" ]; then
        say "$MAC: the turn is done, but the Mac still reports build $WAS_BUILD: put back without the update?"
        audit "$URUN" "$USEQ" "$UFIRST" || true
        exit 1
    fi
    sleep "$POLL"
done
record "$MAC-host-after.json" "$host"
printf '%s' "$host" | jq -r "$FACTS"
if ! version_ge "$NOW_VERSION" "$TO"; then
    say "$MAC: the Mac reports macOS $NOW_VERSION ($NOW_BUILD), older than the $TO that was due"
    audit "$URUN" "$USEQ" "$UFIRST" || true
    exit 1
fi
[ "$NOW_BUILD" = "$BUILD" ] ||
    say "$MAC: note: build $NOW_BUILD, not the $BUILD due at the schedule: the apply takes the newest offered then"
AFTER="sw_vers after (the Mac's report): $NOW_VERSION ($NOW_BUILD)"
say "$MAC: $AFTER"
audit "$URUN" "$USEQ" "$UFIRST" || true

# 2. The probe: the Mac drains and is put back again, and sw_vers is read as root on it.
schedule probe "$PROBE_REASON" || exit 1
PRUN=$RUN
follow "$PRUN" probe || exit 1
record "$MAC-run-$PRUN.json" "$GOT"
case $STEP in
    *_DONE) ;;
    *)
        ended_badly "$PRUN" probe "$SEQ" "$FIRST"
        exit 1
        ;;
esac
tries=0
while :; do
    if host=$(read_hk hosts get --node-id "$MAC") &&
        printf '%s' "$host" | jq -e --arg r "$PRUN" '.host.maintenance.probe.runId == $r' >/dev/null; then
        break
    fi
    tries=$((tries + 1))
    if [ "$tries" -ge "$LOOK_TRIES" ]; then
        say "$MAC: the probe's turn is done, but the Mac's report holds no probe of run $PRUN"
        audit "$PRUN" "$SEQ" "$FIRST" || true
        exit 1
    fi
    sleep "$POLL"
done
record "$MAC-host-probed.json" "$host"
PROBED=$(printf '%s' "$host" | jq -r '.host.maintenance.probe.steps[]?
    | "  probe \(.name): exit \(.exitCode // "-"): \(.output // "")\(if .cut then " (cut)" else "" end)"')
say "$PROBED"
audit "$PRUN" "$SEQ" "$FIRST" || true
record "$MAC-audit.txt" "$AUDIT"
record "$MAC-sw_vers.txt" "$BEFORE
$AFTER
sw_vers after (the probe, as root):
$PROBED"
if ! printf '%s' "$host" | jq -e --arg b "$NOW_BUILD" \
    '[.host.maintenance.probe.steps[]? | select(.name == "sw_vers" and ((.output // "") | contains($b)))] | length > 0' >/dev/null; then
    say "$MAC: the probe's sw_vers does not name build $NOW_BUILD"
    exit 1
fi
say "$MAC: macOS $WAS_VERSION ($WAS_BUILD) -> $NOW_VERSION ($NOW_BUILD) by run $URUN, probed back in service by run $PRUN"
exit 0
