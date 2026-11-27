#!/bin/sh
# Held: set a host group's standing maintenance policy for its Macs: maintenance.batch,
# min_serving, drain_deadline_secs and return_secs, and macos_major off, and nothing else of the
# group.
#
# Run by the owner only, from an operator box whose `hk` has its settings file
# (`hk settings init`, docs/cli.md), with `jq` and `diff` on PATH. Nothing else runs it.
#
# A group is replaced whole (`hk host-groups put`), so this script never writes a body of its
# own: it reads the group (`hk host-groups get`), changes those keys in the group exactly as the
# coordinator answered it, and proves that the only leaves that differ are among
# policy.maintenance.{batch,minServing,drainDeadlineSecs,returnSecs,macosMajor} before it writes
# anything. The Xcode keys and linux_scope are left as they are (xcode-keep.sh sets the Xcode keys).
#
# The defaults are the policy chosen for mac-build's four Macs
# (docs/operations/maintenance-first-mac-run.md, "The standing policy"):
#   batch 1                    one Mac in a turn at a time;
#   min_serving 2              a turn starts only while three Macs serve, so two always do;
#   drain_deadline_secs 14400  a drain waits four hours for the builds on the Mac, then defers;
#   return_secs 3600           a Mac silent an hour after its restart is not_back;
#   macos_major off            a turn takes only the newest minor update of the running major.
# A key of 0 (drain deadline, return) is stored as no key: no deadline, the platform's default.
# A group with fewer hosts than min_serving + 1 never starts a turn, and is refused.
#
# Without --apply (the default) it changes nothing: it reads the group and the open maintenance
# runs, prints the group's maintenance table before and after, the leaves that differ and, with
# --record, a unified diff of the two bodies, and prints the one command --apply would run as
# `DRY-RUN:`.
#
# With --apply (and --record, which keeps the body to restore): the same reads and checks, then
# one `hk host-groups put --if-generation <the generation read>`, which the coordinator refuses
# generation_mismatch when the group changed since the read. Then it checks the put's audit
# entry says `policy maintenance: ...` and nothing more, reads the group again and compares it
# whole with the body it sent, and prints the line that puts the old body back.
#
# It refuses (exit 1) while a maintenance run that is not ended names the group or one of its
# hosts, so a run never reads the keys mid-turn.
#
# Every read and change is printed before it runs: `READ:` a read, `+` a change.
#
# Exit: 0 the change is shown (dry run), made, or already in place; 1 a refusal or a check that
# failed; 2 a usage error.

set -eu

APPLY=0
GROUP=mac-build
BATCH=1
MIN_SERVING=2
DRAIN=14400
RETURN=3600
RECORD=""
REASON=""

usage() {
    cat <<'EOF'
usage: sh mac-policy.sh [--group NAME] [--batch N] [--min-serving N] [--drain-deadline-secs N]
                        [--return-secs N] [--apply] [--record DIR] [--reason TEXT]

  --group NAME             the host group (default mac-build)
  --batch N                hosts in a turn at once, 1 to 10000 (default 1)
                           (each number is plain decimal, with no leading zero)
  --min-serving N          hosts that keep serving, 0 to 10000 (default 2)
  --drain-deadline-secs N  how long a drain waits, 0 for no deadline (default 14400)
  --return-secs N          how long a restarted host may stay silent, 60 to 86400, or 0 for the
                           platform's default of 3600 on a Mac (default 3600)
  --apply                  put the group; without it nothing is changed
  --record DIR             write the group before and after into DIR (it must exist); needed
                           with --apply, since the restore line puts the body kept there
  --reason TEXT            the put's reason (default: "Standing maintenance policy of <group>")

macos_major is always set off (left out of the table): this policy takes minor updates only.
EOF
}

die_usage() {
    printf 'mac-policy: %s\n' "$1" >&2
    usage >&2
    exit 2
}

is_whole() {
    case $1 in
        '' | *[!0-9]* | 0?*) return 1 ;;
        *) [ "${#1}" -le 9 ] ;;
    esac
}

while [ $# -gt 0 ]; do
    case $1 in
        --apply) APPLY=1 ;;
        --group | --batch | --min-serving | --drain-deadline-secs | --return-secs | --record | --reason)
            [ $# -ge 2 ] || die_usage "$1 needs a value"
            case $1 in
                --group) GROUP=$2 ;;
                --batch) BATCH=$2 ;;
                --min-serving) MIN_SERVING=$2 ;;
                --drain-deadline-secs) DRAIN=$2 ;;
                --return-secs) RETURN=$2 ;;
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
# The coordinator's own ranges (maintenance_policy.rs).
is_whole "$BATCH" && [ "$BATCH" -ge 1 ] && [ "$BATCH" -le 10000 ] ||
    die_usage "--batch is 1 to 10000: $BATCH"
is_whole "$MIN_SERVING" && [ "$MIN_SERVING" -le 10000 ] ||
    die_usage "--min-serving is 0 to 10000: $MIN_SERVING"
is_whole "$DRAIN" || die_usage "--drain-deadline-secs is a whole number of seconds: $DRAIN"
is_whole "$RETURN" && { [ "$RETURN" -eq 0 ] || { [ "$RETURN" -ge 60 ] && [ "$RETURN" -le 86400 ]; }; } ||
    die_usage "--return-secs is 0, or 60 to 86400: $RETURN"
case $REASON in
    ' '* | *' ' | "$(printf '\t')"* | *"$(printf '\t')") die_usage "--reason has a space at an end, which the coordinator trims from its audit entry" ;;
esac
case $RECORD in
    '' | /*) ;;
    *) [ ! -d "$RECORD" ] || RECORD="$(cd "$RECORD" && pwd)" ;;
esac
if [ -n "$RECORD" ] && [ ! -d "$RECORD" ]; then
    die_usage "--record names no directory: $RECORD"
fi
[ "$APPLY" -eq 0 ] || [ -n "$RECORD" ] || die_usage "--apply needs --record: the body to restore is kept there"
for tool in hk jq diff grep; do
    command -v "$tool" >/dev/null 2>&1 || die_usage "$tool is not on PATH"
done
[ -n "$REASON" ] || REASON="Standing maintenance policy of $GROUP"

say() { printf '%s\n' "$*"; }

read_hk() {
    say "READ: hk $*" >&2
    hk "$@"
}

change_hk() {
    say "+ hk $*" >&2
    hk "$@"
}

# keep_file <name> <text>: writes $RECORD/<name>, refusing a file already there.
keep_file() {
    if [ -e "$RECORD/$1" ]; then
        say "mac-policy: $RECORD/$1 is there already; give an empty --record directory"
        exit 1
    fi
    printf '%s\n' "$2" >"$RECORD/$1"
}

got=$(read_hk host-groups get --host-group "$GROUP") || { say "the group could not be read"; exit 1; }
GEN=$(printf '%s' "$got" | jq -r '.hostGroup.generation // empty')
OLD=$(printf '%s' "$got" | jq -c '.hostGroup.group // empty')
if [ -z "$GEN" ] || [ -z "$OLD" ]; then
    say "the answer holds no group and generation: $got"
    exit 1
fi
if printf '%s' "$got" | jq -e '(.nextPageToken // "") != ""' >/dev/null; then
    say "the group has more hosts than one page; the run check below would miss some"
    exit 1
fi
NODES=$(printf '%s' "$got" | jq -c '.nodeIds // []')
COUNT=$(printf '%s' "$NODES" | jq 'length')
say "group $GROUP, generation $GEN, $COUNT hosts: $(printf '%s' "$NODES" | jq -r 'join(" ")')"
if [ "$COUNT" -lt $((MIN_SERVING + 1)) ]; then
    say "refused: $COUNT hosts with min_serving $MIN_SERVING: a turn starts only while $((MIN_SERVING + 1)) serve, so none would ever start"
    exit 1
fi
if [ "$COUNT" -lt $((MIN_SERVING + BATCH)) ]; then
    say "note: with $COUNT hosts and min_serving $MIN_SERVING, at most $((COUNT - MIN_SERVING)) are in a turn at once, not $BATCH"
fi

NEW=$(printf '%s' "$OLD" | jq -c --argjson b "$BATCH" --argjson m "$MIN_SERVING" \
    --argjson d "$DRAIN" --argjson r "$RETURN" '
    .policy.maintenance.batch = $b | .policy.maintenance.minServing = $m
    | .policy.maintenance.drainDeadlineSecs = ($d | tostring)
    | .policy.maintenance.returnSecs = ($r | tostring)
    | del(.policy.maintenance.macosMajor)
    | if $d == 0 then del(.policy.maintenance.drainDeadlineSecs) else . end
    | if $r == 0 then del(.policy.maintenance.returnSecs) else . end')

# Every leaf that differs between the two bodies, by its path; each must be one this script sets.
CHANGED=$(jq -cn --argjson a "$OLD" --argjson b "$NEW" '
    [$a, $b | paths(scalars)] | unique
    | map(select(. as $p | ($a | getpath($p)) != ($b | getpath($p))) | map(tostring) | join("."))')
STRAY=$(printf '%s' "$CHANGED" | jq -c 'map(select(IN("policy.maintenance.batch",
    "policy.maintenance.minServing", "policy.maintenance.drainDeadlineSecs",
    "policy.maintenance.returnSecs", "policy.maintenance.macosMajor") | not))')
say "maintenance before: $(printf '%s' "$OLD" | jq -c '.policy.maintenance // {}')"
say "maintenance after:  $(printf '%s' "$NEW" | jq -c '.policy.maintenance // {}')"
say "leaves that differ: $CHANGED"
if [ "$STRAY" != "[]" ]; then
    say "refused: the change touches more than the policy keys: $STRAY"
    exit 1
fi
if [ "$CHANGED" = "[]" ]; then
    say "the group has this policy already; nothing to do"
    exit 0
fi

# A run that is not ended and names the group, or one of its hosts, refuses the change.
runs=$(read_hk maintenance list --states scheduled --states running --states halted) ||
    { say "the maintenance runs could not be read"; exit 1; }
open=$(printf '%s' "$runs" | jq -r --arg g "$GROUP" --argjson nodes "$NODES" '.runs[]?
    | select(.hostGroup == $g or ((.hostIds // []) | any(. as $h | $nodes | index($h))))
    | "\(.id) \(.state)"')
if [ -n "$open" ]; then
    say "refused: a maintenance run is open on the group or its hosts:"
    say "$open"
    exit 1
fi

BEFORE=$(jq -cn --argjson g "$OLD" '{hostGroup: $g}')
AFTER=$(jq -cn --argjson g "$NEW" '{hostGroup: $g}')
if [ -n "$RECORD" ]; then
    tag=dry
    [ "$APPLY" -eq 0 ] || tag=apply
    keep_file "$GROUP-before-$tag.json" "$(printf '%s' "$BEFORE" | jq -S .)"
    keep_file "$GROUP-after-$tag.json" "$(printf '%s' "$AFTER" | jq -S .)"
    diff -u "$RECORD/$GROUP-before-$tag.json" "$RECORD/$GROUP-after-$tag.json" || true
fi

if [ "$APPLY" -eq 0 ]; then
    say "DRY-RUN: hk host-groups put --request <the after body> --if-generation $GEN --reason \"$REASON\""
    exit 0
fi

put=$(printf '%s' "$AFTER" | change_hk host-groups put --request - --if-generation "$GEN" --reason "$REASON") ||
    { say "the put was refused (its reason is above); nothing changed"; exit 1; }
keep_file "$GROUP-put.json" "$put"
NEWGEN=$(printf '%s' "$put" | jq -r '.generation // empty')
said=$(printf '%s' "$put" | jq -r '.entry.reason // ""')
say "audit $(printf '%s' "$put" | jq -r '.entry | "\(.seq) \(.atUnix // "-") \(.action) by \(.byName // .by) group \(.hostGroup // "-")"'): $said"
restore="hk host-groups put --request \"$RECORD/$GROUP-before-apply.json\" --if-generation $NEWGEN --reason \"Restore $GROUP's maintenance policy\""
case $said in
    "policy maintenance: $REASON") ;;
    *)
        say "the put's audit entry names more than the maintenance table: $said"
        say "to put the old body back: $restore"
        exit 1
        ;;
esac

again=$(read_hk host-groups get --host-group "$GROUP") || { say "the group could not be read again"; exit 1; }
keep_file "$GROUP-after-read.json" "$again"
# The coordinator answers the four keys it always sets (xcode_default, linux_scope, batch,
# min_serving) even where the body sent had no table; both sides are compared with them filled in.
if ! printf '%s' "$again" | jq -e --argjson want "$NEW" --arg gen "$NEWGEN" '
    def filled: .policy.maintenance |= ({xcodeDefault: "latest", linuxScope: "LINUX_SCOPE_SECURITY",
        batch: 1, minServing: 1} + (. // {}));
    .hostGroup.generation == $gen and (.hostGroup.group | filled) == ($want | filled)' >/dev/null; then
    say "the group read again is not the body put at generation $NEWGEN; read \"$RECORD/$GROUP-after-read.json\""
    say "to put the old body back: $restore"
    exit 1
fi
say "$GROUP has batch $BATCH, min_serving $MIN_SERVING, drain_deadline_secs $DRAIN, return_secs $RETURN, macos_major off, generation $NEWGEN"
say "to put the old body back: $restore"
exit 0
