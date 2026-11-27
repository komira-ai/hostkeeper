#!/bin/sh
# Held: read what each Mac of a host group reports for a macOS update, and say what stops one.
#
# Run by the owner only, from an operator box whose `hk` has its settings file
# (`hk settings init`, docs/cli.md), with `jq` and `awk` on PATH. Nothing else runs it.
#
# It changes nothing, and has no --apply: it reads the group (`hk host-groups get`), each Mac
# (`hk hosts get`) and the coordinator's Schedule preview of a macOS turn on that Mac alone (a
# `maintenance schedule --kinds macos` without `--confirm`, which stores nothing and writes no
# audit entry). For each Mac it prints the facts a macOS turn depends on:
#   - the update account (present, admin, secure token; hidden is shown and gates nothing);
#   - FileVault and whether it supports an authenticated restart;
#   - what device management sets that an update conflicts with (the coordinator's mdm_conflict);
#   - the macOS it runs and the macOS updates due on it;
#   - the daemon's release, whether its report is fresh, and whether it takes a probe;
# then one line, `<mac>: ready` or `<mac>: gaps: ...`, naming each thing that stops a macOS turn,
# in the order the coordinator tests them (docs/operations/maintenance.md, "The Mac: macOS and
# Xcode"). The group's maintenance table is printed once, and a group whose macos_major is true
# is a gap of every Mac: a turn would then take a new major instead of the minor update.
#
# Every read is printed before it runs, as `READ:`. With --record DIR (an existing directory, the
# workspace scratch directory and not /tmp) each answer is also written there as
# <mac>-<what>.json; a file already there is never overwritten.
#
# Exit: 0 every Mac read is ready for a macOS turn; 1 a Mac has a gap or could not be read;
# 2 a usage error.

set -eu

MIN_DAEMON=0.1.480
GROUP=mac-build
MACS=""
RECORD=""

usage() {
    cat <<'EOF'
usage: sh mac-facts.sh [--group NAME] [--mac ID]... [--record DIR]

  --group NAME   the host group (default mac-build); its hosts are read unless --mac is given
  --mac ID       a Mac by node id, repeatable, read in the order given
  --record DIR   write each answer into DIR (it must exist)
EOF
}

die_usage() {
    printf 'mac-facts: %s\n' "$1" >&2
    usage >&2
    exit 2
}

while [ $# -gt 0 ]; do
    case $1 in
        --group)
            [ $# -ge 2 ] || die_usage "--group needs a value"
            GROUP=$2
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
for mac in $MACS; do
    case $mac in
        -* | *[!a-z0-9-]*) die_usage "--mac is a node id of [a-z0-9-]: $mac" ;;
    esac
done
if [ -n "$RECORD" ] && [ ! -d "$RECORD" ]; then
    die_usage "--record names no directory: $RECORD"
fi
for tool in hk jq awk; do
    command -v "$tool" >/dev/null 2>&1 || die_usage "$tool is not on PATH"
done

say() { printf '%s\n' "$*"; }

read_hk() {
    say "READ: hk $*" >&2
    hk "$@"
}

record() {
    [ -n "$RECORD" ] || return 0
    if [ -e "$RECORD/$1" ]; then
        say "mac-facts: $RECORD/$1 is there already; not written" >&2
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

# The facts of one `hk hosts get` answer, one line each.
FACTS='.host as $h | ($h.maintenance // {}) as $m | ($m.mac // {}) as $x | ($x.updateAccount // {}) as $a |
    "  group \($h.hostGroup // "-"), live \(($h | .live) // false), daemon \($h.heartbeat.agent.version // "-")",
    "  macOS \($m.host.osVersion // "-") (\($m.host.osBuild // "-"))",
    "  update account: present \($a.present // false), admin \($a.admin // false), secure token \($a.secureToken // false), hidden \($a.hidden // false)",
    "  FileVault \(($x.filevault // "FILE_VAULT_STATE_UNSPECIFIED") | sub("FILE_VAULT_STATE_"; "") | ascii_downcase), authrestart supported \($x.authrestartSupported // false)",
    "  device management conflicts: \(($x.mdmConflicts // []) | if . == [] then "none" else join(", ") end)",
    "  report: look age \($h.maintenanceLookAgeSecs // "-") s, stale \($h.maintenanceStale // false), look failed \(($m.lookFailed // "") | if . == "" then "no" else . end)",
    "  free on the data volume: \($x.dataFreeBytes // "-") bytes",
    "  takes on demand: \(($m.onDemand // []) | map(sub("MAINTENANCE_KIND_"; "") | ascii_downcase) | if . == [] then "nothing" else join(", ") end)",
    (if [($m.due // [])[] | select(.kind == "MAINTENANCE_KIND_MACOS")] == [] then "  due macOS: nothing" else
        (($m.due // [])[] | select(.kind == "MAINTENANCE_KIND_MACOS")
            | "  due macOS \(.from // "-") -> \(.to // "-") \(.label // "")\(if .needsReboot then ", restarts" else "" end)\(if ((.from // "") | split(".")[0]) != ((.to // "") | split(".")[0]) then ", a new major: not taken" else "" end)") end),
    (if ($m | .run) then "  in run \(($m | .run).runId) at \(($m | .run).step)" else empty end)'

# What stops a macOS turn on the Mac, by the coordinator's rules, one line each; none: ready. A
# due item counts only within the running major (from and to share their first number), as the
# coordinator counts it while the group's macos_major is off (maintenance_mac.rs, counts_as_due).
GAPS='def minor: ((.from // "") | split(".")[0]) == ((.to // "") | split(".")[0]); .host as $h | ($h.maintenance // null) as $m |
    (if (($h | .live) // false) | not then "not live" else empty end),
    (if $m == null then "no maintenance report: its daemon has not announced maintenance-v1" else
        ((if ($h.maintenanceStale // false) then "its report is stale" else empty end),
         (if ($m.lookFailed // "") != "" then "its last look failed: \($m.lookFailed)" else empty end),
         (if $m.mac == null then "no_update_account: its report holds no Mac facts" else
            ($m.mac as $x | ($x.updateAccount // {}) as $a |
             (if ($x.mdmConflicts // []) != [] then "mdm_conflict: device management sets \($x.mdmConflicts | join(", "))" else empty end),
             (if (($a.present // false) and ($a.admin // false) and ($a.secureToken // false)) | not then
                "no_update_account: present \($a.present // false), admin \($a.admin // false), secure token \($a.secureToken // false)" else empty end),
             (if ($x.filevault // "") != "FILE_VAULT_STATE_OFF" then
                "filevault_not_ready: FileVault \(($x.filevault // "FILE_VAULT_STATE_UNSPECIFIED") | sub("FILE_VAULT_STATE_"; "") | ascii_downcase); no run orders macos on a Mac whose FileVault is not off" else empty end)) end),
         (if [($m.due // [])[] | select(.kind == "MAINTENANCE_KIND_MACOS" and minor)] == [] then "no macOS update of the running major due" else empty end))
    end)'

grp=$(read_hk host-groups get --host-group "$GROUP") || { say "the group $GROUP could not be read"; exit 1; }
record "$GROUP-group.json" "$grp"
if printf '%s' "$grp" | jq -e '(.nextPageToken // "") != ""' >/dev/null; then
    say "the group has more hosts than one page; name the Macs with --mac"
    exit 1
fi
NODES=$(printf '%s' "$grp" | jq -r '(.nodeIds // []) | join(" ")')
MAJOR=$(printf '%s' "$grp" | jq -r '.hostGroup.group.policy.maintenance.macosMajor // false')
say "group $GROUP: hosts ${NODES:-none}"
say "  maintenance: $(printf '%s' "$grp" | jq -c '.hostGroup.group.policy.maintenance // {}')"
[ -n "$MACS" ] || MACS=$NODES
[ -n "$MACS" ] || { say "the group $GROUP has no hosts"; exit 1; }

status=0
for mac in $MACS; do
    if ! host=$(read_hk hosts get --node-id "$mac"); then
        say "$mac: gaps: could not be read (hosts get refused, reason above)"
        status=1
        continue
    fi
    record "$mac-host.json" "$host"
    say "$mac:"
    printf '%s' "$host" | jq -r "$FACTS"
    gaps=$(printf '%s' "$host" | jq -r "$GAPS")
    daemon=$(printf '%s' "$host" | jq -r '.host.heartbeat.agent.version // ""')
    if [ -z "$daemon" ] || ! version_ge "$daemon" "$MIN_DAEMON"; then
        gaps="$gaps
daemon ${daemon:-unknown} is older than $MIN_DAEMON"
    fi
    if ! printf '%s' "$host" | jq -e '(.host.maintenance.onDemand // []) | index("MAINTENANCE_KIND_PROBE")' >/dev/null; then
        gaps="$gaps
takes no probe: the check after the update cannot run"
    fi
    if [ "$(printf '%s' "$host" | jq -r '.host.hostGroup // ""')" != "$GROUP" ]; then
        gaps="$gaps
not in the group $GROUP"
    fi
    if [ "$MAJOR" = true ]; then
        gaps="$gaps
the group's macos_major is true: a turn would take a new major in place of the minor update"
    fi
    # The coordinator's own verdict, which stores nothing: waiting, skipped (and why), or refused.
    if preview=$(read_hk maintenance schedule --kinds macos --host-ids "$mac"); then
        record "$mac-preview.json" "$preview"
        verdict=$(printf '%s' "$preview" | jq -r --arg m "$mac" '[.hosts[]? | select(.hostId == $m)][0] // {}
            | "\(.step // "-" | sub("MAINTENANCE_HOST_STEP_"; "") | ascii_downcase)\(if .skippedBecause then " " + (.skippedBecause | sub("MAINTENANCE_SKIP_"; "") | ascii_downcase) else "" end)"')
        say "  preview: $verdict"
        case $verdict in
            waiting) ;;
            *) gaps="$gaps
the preview takes no macos turn on it: $verdict" ;;
        esac
    else
        say "  preview: refused (its reason is above)"
        gaps="$gaps
the preview was refused"
    fi
    gaps=$(printf '%s\n' "$gaps" | awk 'NF' | awk 'BEGIN { ORS = "" } { if (NR > 1) print "; "; print }')
    if [ -z "$gaps" ]; then
        say "$mac: ready"
    else
        say "$mac: gaps: $gaps"
        status=1
    fi
done
exit "$status"
