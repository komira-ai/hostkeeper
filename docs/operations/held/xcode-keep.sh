#!/bin/sh
# Held: set one host group's maintenance.xcode_keep and maintenance.xcode_default, and nothing
# else of the group.
#
# Run by the owner only, from an operator box whose `hk` has its settings file
# (`hk settings init`, docs/cli.md), with `jq` and `diff` on PATH. Nothing else runs it.
#
# A group is replaced whole (`hk host-groups put`), so this script never writes a body of its
# own: it reads the group (`hk host-groups get`), changes the two keys in the group exactly as
# the coordinator answered it, and proves that the only leaves that differ are
# policy.maintenance.xcodeKeep and policy.maintenance.xcodeDefault before it writes anything.
#
# --default is needed with a --keep above 0, so a group never keeps Xcode with a default nobody
# chose: `latest` or a version the group keeps has each Mac's turn point xcode-select at that
# Xcode, which moves every build on the Mac that names no Xcode onto it. A version the store
# keeps no release of (`0`: no Xcode release is 0) has the order carry no default, and the turn
# installs each kept release beside the tools and leaves xcode-select as it is
# (docs/operations/maintenance.md, "The private store").
#
# Without --apply (the default) it changes nothing: it reads the group and the open maintenance
# runs, prints the group's maintenance table before and after, the leaves that differ and, with
# --record, a unified diff of the two bodies, and prints the one command --apply would run as
# `DRY-RUN:`.
#
# With --apply (and --record, which keeps the body to restore): the same reads and checks, then
# one `hk host-groups put --if-generation <the generation read>`, which the coordinator refuses
# generation_mismatch when the group changed since the read. Then it checks the put's audit
# entry says `policy maintenance: ...` and nothing more (the coordinator names each part of a
# group a put changed), reads the group again and compares it whole with the body it sent, and
# prints the line that puts the old body back.
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
GROUP=""
KEEP=""
DEFAULT=""
RECORD=""
REASON=""

usage() {
    cat <<'EOF'
usage: sh xcode-keep.sh --group NAME [--keep N] [--default D] [--apply] [--record DIR]
                        [--reason TEXT]

  --group NAME   the host group (mac-build)
  --keep N       the xcode_keep to set, 0 to 4 (default: the group's, unchanged)
  --default D    the xcode_default to set: latest, or a version of 1 to 3 numbers of 1 to 4
                 digits; needed with a --keep above 0. 0 names no Xcode the store holds, so
                 the turns install Xcode and leave xcode-select as it is
  --apply        put the group; without it nothing is changed
  --record DIR   write the group before and after into DIR (it must exist); needed with --apply,
                 since the restore line puts the body kept there
  --reason TEXT  the put's reason (default: "Set xcode_keep <N> and xcode_default <D> on <group>")
EOF
}

die_usage() {
    printf 'xcode-keep: %s\n' "$1" >&2
    usage >&2
    exit 2
}

while [ $# -gt 0 ]; do
    case $1 in
        --apply) APPLY=1 ;;
        --group)
            [ $# -ge 2 ] || die_usage "--group needs a value"
            GROUP=$2
            shift
            ;;
        --keep)
            [ $# -ge 2 ] || die_usage "--keep needs a value"
            KEEP=$2
            shift
            ;;
        --default)
            [ $# -ge 2 ] || die_usage "--default needs a value"
            DEFAULT=$2
            shift
            ;;
        --record)
            [ $# -ge 2 ] || die_usage "--record needs a value"
            RECORD=$2
            shift
            ;;
        --reason)
            [ $# -ge 2 ] || die_usage "--reason needs a value"
            REASON=$2
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
    '') die_usage "--group is needed" ;;
    -* | *[!a-z0-9-]*) die_usage "--group is a group name of [a-z0-9-]: $GROUP" ;;
esac
case $KEEP in
    '' | 0 | 1 | 2 | 3 | 4) ;;
    *) die_usage "--keep is 0 to 4: $KEEP" ;;
esac
case $REASON in
    ' '* | *' ' | "$(printf '\t')"* | *"$(printf '\t')") die_usage "--reason has a space at an end, which the coordinator trims from its audit entry" ;;
esac
# The coordinator's own rule for xcode_default (maintenance_policy.rs): latest, or 1 to 3
# numbers of 1 to 4 digits joined by dots.
if [ -n "$DEFAULT" ] && [ "$DEFAULT" != latest ] &&
    ! printf '%s' "$DEFAULT" | grep -Eqx '[0-9]{1,4}(\.[0-9]{1,4}){0,2}'; then
    die_usage "--default is latest or a version of 1 to 3 numbers of 1 to 4 digits: $DEFAULT"
fi
if [ -n "$KEEP" ] && [ "$KEEP" -gt 0 ] && [ -z "$DEFAULT" ]; then
    die_usage "--keep $KEEP needs --default: latest or a kept version moves xcode-select, 0 leaves it"
fi
if [ "$KEEP" = 0 ] && [ -n "$DEFAULT" ] && [ "$DEFAULT" != latest ]; then
    die_usage "--keep 0 keeps no Xcode, so --default can only be latest (the coordinator refuses a version)"
fi
[ -n "$KEEP" ] || [ -n "$DEFAULT" ] || die_usage "give --keep, --default or both"
case $RECORD in
    '' | /*) ;;
    *) RECORD="$(cd "$RECORD" && pwd)" ;;
esac
if [ -n "$RECORD" ] && [ ! -d "$RECORD" ]; then
    die_usage "--record names no directory: $RECORD"
fi
[ "$APPLY" -eq 0 ] || [ -n "$RECORD" ] || die_usage "--apply needs --record: the body to restore is kept there"
for tool in hk jq diff grep; do
    command -v "$tool" >/dev/null 2>&1 || die_usage "$tool is not on PATH"
done

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
        say "xcode-keep: $RECORD/$1 is there already; give an empty --record directory"
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
NODES=$(printf '%s' "$got" | jq -c '.nodeIds // []')
if printf '%s' "$got" | jq -e '(.nextPageToken // "") != ""' >/dev/null; then
    say "the group has more hosts than one page; the run check below would miss some"
    exit 1
fi
HAD=$(printf '%s' "$OLD" | jq -r '.policy.maintenance.xcodeKeep // 0')
HAD_DEFAULT=$(printf '%s' "$OLD" | jq -r '.policy.maintenance.xcodeDefault // "latest"')
KEEP=${KEEP:-$HAD}
WANT_DEFAULT=${DEFAULT:-$HAD_DEFAULT}
if [ "$KEEP" -gt 0 ] && [ -z "$DEFAULT" ] && [ "$HAD" -eq 0 ]; then
    say "refused: the group keeps no Xcode yet, so give --default: latest or a kept version moves xcode-select, 0 leaves it"
    exit 1
fi
[ -n "$REASON" ] || REASON="Set xcode_keep $KEEP and xcode_default $WANT_DEFAULT on $GROUP"
if [ "$KEEP" -eq 0 ] && [ "$WANT_DEFAULT" != latest ]; then
    say "refused: the group's xcode_default is $WANT_DEFAULT, which an xcode_keep of 0 does not take; give --default latest"
    exit 1
fi
say "group $GROUP, generation $GEN, hosts $(printf '%s' "$NODES" | jq -r 'join(" ")')"
say "xcode_keep $HAD -> $KEEP, xcode_default $HAD_DEFAULT -> $WANT_DEFAULT"

if [ "$HAD" = "$KEEP" ] && [ "$HAD_DEFAULT" = "$WANT_DEFAULT" ]; then
    say "the group has xcode_keep $KEEP and xcode_default $WANT_DEFAULT already; nothing to do"
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

NEW=$(printf '%s' "$OLD" | jq -c --argjson k "$KEEP" --arg d "$WANT_DEFAULT" '
    .policy.maintenance.xcodeKeep = $k | .policy.maintenance.xcodeDefault = $d
    | if $k == 0 then del(.policy.maintenance.xcodeKeep) else . end')

# Every leaf that differs between the two bodies, by its path; it must be exactly the keys that
# change, of the two this script sets.
CHANGED=$(jq -cn --argjson a "$OLD" --argjson b "$NEW" '
    [$a, $b | paths(scalars)] | unique
    | map(select(. as $p | ($a | getpath($p)) != ($b | getpath($p))) | map(tostring) | join("."))')
WANTED=$(jq -cn --arg hk "$HAD" --arg k "$KEEP" --arg hd "$HAD_DEFAULT" --arg d "$WANT_DEFAULT" \
    --argjson had_default "$(printf '%s' "$OLD" | jq '(.policy.maintenance // {}) | has("xcodeDefault")')" '
    [(if $hd != $d or ($had_default | not) then "policy.maintenance.xcodeDefault" else empty end),
     (if $hk != $k then "policy.maintenance.xcodeKeep" else empty end)]')
say "maintenance before: $(printf '%s' "$OLD" | jq -c '.policy.maintenance // {}')"
say "maintenance after:  $(printf '%s' "$NEW" | jq -c '.policy.maintenance // {}')"
say "leaves that differ: $CHANGED"
if [ "$CHANGED" != "$WANTED" ]; then
    say "refused: the change is not exactly $WANTED"
    exit 1
fi
case $WANT_DEFAULT in
    latest) say "note: xcode_default latest has each Mac's turn point xcode-select at the newest kept Xcode" ;;
    *) say "note: xcode_default $WANT_DEFAULT: while the store keeps no release of it, the turns install Xcode and leave xcode-select as it is" ;;
esac

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
restore="hk host-groups put --request \"$RECORD/$GROUP-before-apply.json\" --if-generation $NEWGEN --reason \"Restore $GROUP's xcode_keep $HAD and xcode_default $HAD_DEFAULT\""
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
if ! printf '%s' "$again" | jq -e --argjson want "$NEW" --arg gen "$NEWGEN" \
    '.hostGroup.generation == $gen and .hostGroup.group == $want' >/dev/null; then
    say "the group read again is not the body put at generation $NEWGEN; read \"$RECORD/$GROUP-after-read.json\""
    say "to put the old body back: $restore"
    exit 1
fi
say "$GROUP has xcode_keep $KEEP and xcode_default $WANT_DEFAULT, generation $NEWGEN"
say "to put the old body back: $restore"
exit 0
