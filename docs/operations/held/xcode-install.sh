#!/bin/sh
# Held: install an Xcode release on farm Macs, one Mac at a time, each through one hostkeeper
# maintenance run of the kind `xcode` (stage, drain, apply, check, put back).
#
# Run by the owner only, from an operator box whose `hk` has its settings file
# (`hk settings init`, docs/cli.md), with `jq` on PATH. Nothing else runs it.
#
# Without --apply (the default) it changes nothing: for each Mac it reads the host
# (`hk hosts get`), the store (`hk xcode list`) and the coordinator's Schedule preview (a
# `maintenance schedule` without `--confirm`, which stores nothing and writes no audit entry),
# says whether the Mac is ready, and prints the one command --apply would run as `DRY-RUN:`.
#
# A Mac is ready only when its turn changes nothing a build on it uses today: its group
# (`hk host-groups get`) must have an xcode_default the store keeps no release of (xcode-keep.sh
# --default 0), so the turn installs /Applications/Xcode-<version>.app and leaves xcode-select as
# it is, and the turn must remove no Xcode (with no default the apply refuses it and the Mac
# halts). After the turn, the Mac's report must not list the new Xcode as the selected one.
#
# With --apply, for each --mac in the order given: the same checks again, then one
# `hk maintenance schedule --kinds xcode --host-ids <mac> --confirm true`, then
# `hk maintenance get` every --poll-secs seconds until the Mac's turn ends, then the Mac's report
# read again until it lists the release by its build, then the audit entries of the run. It stops
# at the first Mac whose turn does not end installed, and never goes on to the next one:
#   deferred      the Mac is back in service and unchanged; the run is cancelled, so it is not
#                 offered again unattended;
#   halted,       the Mac stays drained (cordoned); nothing is done for it: the owner chooses
#   not_back      `maintenance retry-host` or `maintenance put-back`, which are printed;
#   skipped, or   the run took no turn on the Mac; nothing to undo.
#   the run ended
# It never passes --take-last-host, and never retries a refused call.
#
# Every read and change is printed before it runs: `READ:` a read, `+` a change; a run's reads
# are printed once, before the first. The dry run reads every Mac, ready or not.
# With --record DIR (an existing directory, the workspace scratch directory and not /tmp),
# each answer is also written there as <mac>-<what>.json, and the audit lines as
# <mac>-audit.txt; a file already there is never overwritten.
#
# Exit: 0 every Mac ready (dry run) or installed (--apply); 1 a Mac not ready, or a turn that
# did not end installed; 2 a usage error.

set -eu

MIN_DAEMON=0.1.480
APPLY=0
VERSION=27.2
MACS=""
RECORD=""
POLL=15
DRAIN=""
LOOK_TRIES=20
READ_TRIES=40

usage() {
    cat <<'EOF'
usage: sh xcode-install.sh [--apply] [--version V] [--mac ID]... [--record DIR]
                           [--poll-secs N] [--drain-deadline-secs N]

  --version V              the Xcode release, as the store names it (default 27.2)
  --mac ID                 a Mac by node id, repeatable, taken in the order given; the dry run
                           defaults to mac-studio-01 mac-studio-02 mac-studio-03 mac-studio-04,
                           and --apply needs at least one
  --apply                  schedule the runs; without it nothing is changed
  --record DIR             write each answer and the audit lines into DIR (it must exist)
  --poll-secs N            seconds between reads of a run (default 15)
  --drain-deadline-secs N  the run's drain deadline (default: the group's)

The rollout is two invocations: first --mac mac-studio-01 alone, then, once that Mac is
checked, --mac mac-studio-02 --mac mac-studio-03 --mac mac-studio-04.
EOF
}

die_usage() {
    printf 'xcode-install: %s\n' "$1" >&2
    usage >&2
    exit 2
}

is_count() {
    case $1 in
        '' | *[!0-9]*) return 1 ;;
        *) [ "$1" -gt 0 ] ;;
    esac
}

while [ $# -gt 0 ]; do
    case $1 in
        --apply) APPLY=1 ;;
        --version)
            [ $# -ge 2 ] || die_usage "--version needs a value"
            VERSION=$2
            shift
            ;;
        --mac)
            [ $# -ge 2 ] || die_usage "--mac needs a value"
            MACS="$MACS $2"
            shift
            ;;
        --record)
            [ $# -ge 2 ] || die_usage "--record needs a value"
            RECORD=$2
            shift
            ;;
        --poll-secs)
            [ $# -ge 2 ] || die_usage "--poll-secs needs a value"
            POLL=$2
            shift
            ;;
        --drain-deadline-secs)
            [ $# -ge 2 ] || die_usage "--drain-deadline-secs needs a value"
            DRAIN=$2
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

case $VERSION in
    '' | .* | *. | *..* | *[!0-9.]*) die_usage "--version is digits and dots: $VERSION" ;;
esac
for mac in $MACS; do
    case $mac in
        -* | *[!a-z0-9-]*) die_usage "--mac is a node id of [a-z0-9-]: $mac" ;;
    esac
done
is_count "$POLL" || die_usage "--poll-secs is a whole number above 0: $POLL"
[ -z "$DRAIN" ] || is_count "$DRAIN" || die_usage "--drain-deadline-secs is a whole number above 0: $DRAIN"
if [ -n "$RECORD" ] && [ ! -d "$RECORD" ]; then
    die_usage "--record names no directory: $RECORD"
fi
if [ -z "$MACS" ]; then
    [ "$APPLY" -eq 0 ] || die_usage "--apply needs --mac: name the Macs it changes"
    MACS="mac-studio-01 mac-studio-02 mac-studio-03 mac-studio-04"
fi
for tool in hk jq awk; do
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
        say "xcode-install: $RECORD/$1 is there already; not written" >&2
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

# The store must hold the release: its build and size are what each Mac is checked against.
store=$(read_hk xcode list) || { say "the store could not be read"; exit 1; }
BUILD=$(printf '%s' "$store" | jq -r --arg v "$VERSION" \
    '[.archives[]? | select(.version == $v)][0].build // empty')
SIZE=$(printf '%s' "$store" | jq -r --arg v "$VERSION" \
    '[.archives[]? | select(.version == $v)][0].sizeBytes // "0"')
BUILDS=$(printf '%s' "$store" | jq -r --arg v "$VERSION" '[.archives[]? | select(.version == $v)] | length')
if [ "$BUILDS" -gt 1 ]; then
    say "the store holds $BUILDS builds of Xcode $VERSION: remove the one not wanted first (hk xcode remove)"
    exit 1
fi
if [ -z "$BUILD" ]; then
    say "the store holds no Xcode $VERSION: bring it in first (hk xcode add $VERSION)"
    exit 1
fi
say "store: Xcode $VERSION build $BUILD, $SIZE bytes"

# installed <host json>: the Mac lists the release as /Applications/Xcode-<version>.app, by its build.
installed() {
    printf '%s' "$1" | jq -e --arg v "$VERSION" --arg b "$BUILD" \
        '[.host.maintenance.xcodes[]? | select(.version == $v and .build == $b
            and .path == "/Applications/Xcode-\($v).app" and (.foreign | not))] | length > 0' \
        >/dev/null
}

# facts <host json>: what the turn depends on, one line each.
facts() {
    printf '%s' "$1" | jq -r '.host as $h | $h.maintenance as $m |
        "  group \($h.hostGroup // "-"), live \(.host.live // false), daemon \($h.heartbeat.agent.version // "-")",
        "  macOS \($m.host.osVersion // "-") (\($m.host.osBuild // "-")), free \($m.mac.dataFreeBytes // "-") bytes",
        "  look age \($h.maintenanceLookAgeSecs // "-") s, stale \($h.maintenanceStale // false), look failed \($m.lookFailed // "" | if . == "" then "no" else . end)",
        (if ($m.xcodes // []) == [] then "  xcodes: none" else
            ($m.xcodes[] | "  xcode \(.version) \(.build) \(.path)\(if .selected then " selected" else "" end)\(if .foreign then " foreign" else "" end)") end),
        (if ($m.due // []) == [] then "  due: nothing" else
            ($m.due[] | "  due \(.kind | sub("MAINTENANCE_KIND_"; "") | ascii_downcase) \(.from // "-") -> \(.to // "-") \(.label // "")") end),
        (if .host.maintenance.run then "  in run \(.host.maintenance.run.runId) at \(.host.maintenance.run.step)" else empty end)'
}

# ready <mac> <tag>: reads the Mac and the coordinator's preview; 0 ready, 1 not, 3 installed.
ready() {
    mac=$1
    host=$(read_hk hosts get --node-id "$mac") || { say "$mac: not ready: hosts get refused"; return 1; }
    record "$mac-host-$2.json" "$host"
    say "$mac:"
    facts "$host"
    xdue=$(printf '%s' "$host" | jq -r '[.host.maintenance.due[]? | select(.kind == "MAINTENANCE_KIND_XCODE")] | length')
    if installed "$host"; then
        if [ "$xdue" = 0 ]; then
            say "$mac: Xcode $VERSION ($BUILD) is installed already; nothing to do"
            return 3
        fi
        say "$mac: not ready: Xcode $VERSION is installed, but xcode is still due (the group's xcode_default): not this script's turn"
        return 1
    fi
    daemon=$(printf '%s' "$host" | jq -r '.host.heartbeat.agent.version // ""')
    if ! printf '%s' "$host" | jq -e '.host.live == true' >/dev/null; then
        say "$mac: not ready: not live"
        return 1
    fi
    if [ -z "$daemon" ] || ! version_ge "$daemon" "$MIN_DAEMON"; then
        say "$mac: not ready: daemon ${daemon:-unknown} is older than $MIN_DAEMON"
        return 1
    fi
    if ! printf '%s' "$host" | jq -e '(.host.maintenanceStale // false) == false and .host.maintenance' >/dev/null; then
        say "$mac: not ready: its maintenance report is stale or missing"
        return 1
    fi
    if ! printf '%s' "$host" | jq -e --arg v "$VERSION" \
        --arg b "$BUILD" '[.host.maintenance.due[]? | select(.kind == "MAINTENANCE_KIND_XCODE"
            and .to == $v and .label == $b)] | length > 0' >/dev/null; then
        say "$mac: not ready: Xcode $VERSION ($BUILD) is not due on it: its group's xcode_keep is 0 (xcode-keep.sh sets it), or the store"
        return 1
    fi
    # The turn must not change what a build on the Mac uses: the group's default must name no
    # release the store keeps for it (the order then carries no default, and the turn leaves
    # xcode-select as it is), and with no default the turn may remove no Xcode (the apply refuses
    # an order that removes one and names no default, xcode_order_invalid, and the Mac halts).
    group=$(printf '%s' "$host" | jq -r '.host.hostGroup // ""')
    if ! grp=$(read_hk host-groups get --host-group "$group"); then
        say "$mac: not ready: its group ${group:-(none)} could not be read"
        return 1
    fi
    record "$mac-group-$2.json" "$grp"
    plan=$(jq -cn --argjson g "$grp" --argjson s "$store" --argjson h "$host" '
        def v: split(".") | map(tonumber? // 0);
        ($g.hostGroup.group.policy.maintenance // {}) as $m
        | ($m.xcodeKeep // 0) as $keep
        | ($m.xcodeDefault // "latest") as $default
        | [($s.archives // [])[:$keep][] | .version] as $kept
        | {keep: $keep, default: $default, kept: $kept,
           selects: (if $keep == 0 then null
                     elif $default == "latest" then $kept[0]
                     elif ($kept | index($default)) then $default else null end),
           removes: [($h.host.maintenance.xcodes // [])[] | select((.foreign | not)
                     and ($kept | length) > 0 and ((.version | v) < ($kept | map(v) | min))) | .version]}')
    say "  group $group: xcode_keep $(printf '%s' "$plan" | jq -r .keep), xcode_default $(printf '%s' "$plan" | jq -r .default), kept $(printf '%s' "$plan" | jq -r '.kept | join(" ")')"
    selects=$(printf '%s' "$plan" | jq -r '.selects // ""')
    if [ -n "$selects" ]; then
        say "$mac: not ready: the turn would point xcode-select at Xcode $selects, which moves every build on the Mac that names no Xcode; set the group's xcode_default to a version the store keeps none of (xcode-keep.sh --default 0)"
        return 1
    fi
    removes=$(printf '%s' "$plan" | jq -r '.removes | join(" ")')
    if [ -n "$removes" ]; then
        say "$mac: not ready: the turn would remove Xcode $removes with no default, which the apply refuses (xcode_order_invalid) and halts the Mac"
        return 1
    fi
    free=$(printf '%s' "$host" | jq -r '.host.maintenance.mac.dataFreeBytes // "0"')
    if ! awk -v f="$free" -v s="$SIZE" 'BEGIN { exit !(f + 0 >= 4 * s) }'; then
        say "$mac: note: $free bytes free, under four times the archive; the stage will defer free_space_short"
    fi
    if ! preview=$(read_hk maintenance schedule --kinds xcode --host-ids "$mac"); then
        say "$mac: not ready: the coordinator refused the preview (its reason is above)"
        return 1
    fi
    record "$mac-preview-$2.json" "$preview"
    row=$(printf '%s' "$preview" | jq -c --arg m "$mac" '[.hosts[]? | select(.hostId == $m)][0] // {}')
    if ! printf '%s' "$row" | jq -e \
        '.step == "MAINTENANCE_HOST_STEP_WAITING" and .kinds == ["MAINTENANCE_KIND_XCODE"]' >/dev/null; then
        say "$mac: not ready: the preview does not take an xcode turn on it: $row"
        return 1
    fi
    say "$mac: ready"
    return 0
}

# audit <mac> <run id> <seq of the schedule entry>: the run's audit entries, oldest first.
audit() {
    entries=$(read_hk audit list --node-id "$1" --since-seq "$3") || { say "$1: the audit log could not be read"; return 1; }
    lines=$(printf '%s' "$entries" | jq -r --arg t "run $2" '.entries[]?
        | (.reason // "") as $r
        | select($r == $t or ($r | startswith($t + " ")) or ($r | endswith(" " + $t)))
        | "audit \(.seq) \(.atUnix // "-") \(.action) by \(.byName // .by) node \(.nodeId // "-"): \($r)"')
    record "$1-audit.txt" "$4
$lines"
    say "$4"
    say "$lines"
}

# turn <mac>: one run on the Mac, followed to its end; 0 installed, 1 otherwise.
turn() {
    mac=$1
    set -- maintenance schedule --kinds xcode --host-ids "$mac"
    [ -z "$DRAIN" ] || set -- "$@" --drain-deadline-secs "$DRAIN"
    set -- "$@" --confirm true --reason "Install Xcode $VERSION ($BUILD) on $mac"
    resp=$(change_hk "$@") || { say "$mac: the schedule was refused (its reason is above); nothing changed"; return 1; }
    record "$mac-schedule.json" "$resp"
    run=$(printf '%s' "$resp" | jq -r '.run.id // empty')
    seq=$(printf '%s' "$resp" | jq -r '.entry.seq // "0"')
    first=$(printf '%s' "$resp" | jq -r '.entry | "audit \(.seq) \(.atUnix // "-") \(.action) by \(.byName // .by) node -: \(.reason)"')
    [ -n "$run" ] || { say "$mac: the answer names no run: $resp"; return 1; }
    say "$mac: run $run scheduled"

    say "READ: hk maintenance get --run-id $run, every $POLL s until the turn ends" >&2
    last=""
    failed=0
    while :; do
        if got=$(hk maintenance get --run-id "$run"); then
            failed=0
            step=$(printf '%s' "$got" | jq -r --arg m "$mac" '[.hosts[]? | select(.hostId == $m)][0].step // "-"')
            state=$(printf '%s' "$got" | jq -r '.run.state // "-"')
            if [ "$step" != "$last" ]; then
                say "$mac: $(printf '%s' "$step" | sed 's/^MAINTENANCE_HOST_STEP_//' | tr 'A-Z' 'a-z') (run $(printf '%s' "$state" | sed 's/^MAINTENANCE_STATE_//' | tr 'A-Z' 'a-z'))"
                last=$step
            fi
            case $step in
                *_DONE | *_DEFERRED | *_HALTED | *_NOT_BACK | *_NOT_REACHED | *_SKIPPED) break ;;
                *_STAGING | *_DRAINING | *_APPLYING | *_REBOOTING | *_CHECKING | *_PUTTING_BACK) ;;
                *)
                    # Not in its turn: a run that ended, or halted in another host, never starts it.
                    case $state in
                        *_DONE | *_CANCELLED | *_WINDOW_ENDED | *_HALTED) break ;;
                    esac
                    ;;
            esac
        else
            failed=$((failed + 1))
            say "$mac: run $run could not be read ($failed in a row); it goes on, and is read again"
            if [ "$failed" -ge "$READ_TRIES" ]; then
                say "$mac: stopped following run $run after $failed failed reads; the run goes on. Read it with"
                say "  hk maintenance get --run-id $run"
                say "and, once the Mac's turn is halted, retry-host or put-back it as below; or cancel it:"
                say "  hk maintenance cancel --run-id $run --reason \"<why>\""
                return 1
            fi
        fi
        sleep "$POLL"
    done
    record "$mac-run.json" "$got"
    row=$(printf '%s' "$got" | jq -c --arg m "$mac" '[.hosts[]? | select(.hostId == $m)][0] // {}')
    why=$(printf '%s' "$row" | jq -r '.reason // ""')

    case $step in
        *_DONE)
            if ! printf '%s' "$row" | jq -e '(.applied // []) | index("MAINTENANCE_KIND_XCODE")' >/dev/null; then
                say "$mac: done without xcode applied (put back by an operator?): $row"
                audit "$mac" "$run" "$seq" "$first" || true
                return 1
            fi
            ;;
        *_DEFERRED)
            say "$mac: deferred: $why; the Mac is back in service, unchanged"
            change_hk maintenance cancel --run-id "$run" \
                --reason "Xcode $VERSION on $mac deferred ($why); the rollout stops here" >/dev/null ||
                say "$mac: the cancel was refused; cancel run $run by hand"
            audit "$mac" "$run" "$seq" "$first" || true
            return 1
            ;;
        *_HALTED | *_NOT_BACK)
            say "$mac: $(printf '%s' "$step" | sed 's/^MAINTENANCE_HOST_STEP_//' | tr 'A-Z' 'a-z'): $why"
            say "$mac: it stays drained. After reading its log, either:"
            say "  hk maintenance retry-host --run-id $run --host-id $mac --reason \"<why>\""
            say "  hk maintenance put-back --run-id $run --host-id $mac --reason \"<why>\""
            audit "$mac" "$run" "$seq" "$first" || true
            return 1
            ;;
        *)
            say "$mac: the turn ended $step (run $state): $row"
            audit "$mac" "$run" "$seq" "$first" || true
            return 1
            ;;
    esac

    # The daemon looks again after each step; read until the report lists the release.
    tries=0
    while :; do
        if host=$(read_hk hosts get --node-id "$mac") && installed "$host"; then
            break
        fi
        tries=$((tries + 1))
        if [ "$tries" -ge "$LOOK_TRIES" ]; then
            say "$mac: the turn is done, but its report does not list Xcode $VERSION ($BUILD)"
            audit "$mac" "$run" "$seq" "$first" || true
            return 1
        fi
        sleep "$POLL"
    done
    record "$mac-host-after.json" "$host"
    facts "$host"
    if printf '%s' "$host" | jq -e --arg v "$VERSION" '[.host.maintenance.xcodes[]? | select(.version == $v and .selected)] | length > 0' >/dev/null; then
        say "$mac: Xcode $VERSION is installed, but xcode-select names it: the builds on the Mac moved onto it"
        audit "$mac" "$run" "$seq" "$first" || true
        return 1
    fi
    audit "$mac" "$run" "$seq" "$first" || true
    say "$mac: Xcode $VERSION ($BUILD) installed by run $run"
    return 0
}

MODE=dry
[ "$APPLY" -eq 0 ] || MODE=apply
status=0
for mac in $MACS; do
    rc=0
    ready "$mac" "$MODE" || rc=$?
    if [ "$rc" -eq 3 ]; then
        continue
    fi
    if [ "$rc" -ne 0 ]; then
        status=1
        [ "$APPLY" -eq 1 ] && { say "the rollout stops at $mac"; exit 1; }
        continue
    fi
    if [ "$APPLY" -eq 0 ]; then
        drain=""
        [ -z "$DRAIN" ] || drain=" --drain-deadline-secs $DRAIN"
        say "DRY-RUN: hk maintenance schedule --kinds xcode --host-ids $mac$drain --confirm true --reason \"Install Xcode $VERSION ($BUILD) on $mac\""
        continue
    fi
    turn "$mac" || { say "the rollout stops at $mac"; exit 1; }
done
exit "$status"
