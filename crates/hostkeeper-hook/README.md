# hostkeeper-hook

The contract between a host updater and the update hook a service ships, and the harness that
proves a hook keeps it. Why it is shaped this way: [docs/design/hostkeeper-design.md](../../docs/design/hostkeeper-design.md)
(sections 4.7, 5.2 and 5.3). This page states what the code holds.

**The hook is any executable**: a binary or a script, in any language. The updater runs
`<executable> <hook_args> <step> [args]`. The executable and `hook_args` are the `command` of the
entry's manifest: its first element is the absolute path of a file the package installs
(`/usr/lib/pxe/pxe`), and the rest are `hook_args`. The daemon runs that file's sealed copy in the
version's extracted tree (`<version dir>/tree/usr/lib/pxe/pxe`), as docs/design/packages-native.md
section 10.2 says; a service ships no wrapper. Our own services use `["update-hook"]` as the
rest (`pxe update-hook cutover`); it is `[]` for an executable that takes the step first. The harness's `HookCommand::new` takes the same convention as its default. The hook
runs as root, as a transient unit with `RuntimeMaxSec` (the step's `timeout_secs` from the
manifest) and nothing else; it is safe to run because it came, byte for byte, from the package
the host's package manager verified, not because anything confines it. This crate is our convention made
code, not a requirement on a hook's language. It depends on no other crate of this workspace, so
a service outside this repository can depend on it; `crates/xtask/tests/layering` keeps it so. The
harness is behind the feature `conformance`, which a service turns on in its dev-dependencies.

## The contract

| step | arguments | what it does |
|---|---|---|
| `prepare` | `<artifact>`, the staged release directory | check the host can take the update; make the new version ready beside the old; change nothing a client can see |
| `cutover` | | the visible change |
| `verify` | | observe that the new version serves; may be run again and again |
| `rollback` | | undo `prepare` and `cutover` as far as they can be undone |
| `cleanup` | | remove what only the update needed; run only after `verify` succeeded |
| `uninstall` | | not a step of an update: run once, from the installed release, when the service is removed from the host; bounded by the manifest's `[hook.timeout_secs] uninstall`, and never run for a manifest without that bound |

Every step may be followed by `--params <file>`: a TOML table of scalars, the
manifest's `[params]` table, opaque to the updater. It also carries the reserved key
`"updater.promotion_ceiling_secs"` (quoted: it is one key with a dot in it), the ceiling the updater
applies to a pending promotion and the hook must not fail for. The updater writes four more
reserved keys, all in the namespace `updater.`, which a hook only reads: `updater.staged_release`
(the absolute path of the verified staged release tree; equal to the argument of `prepare`),
`updater.previous_release` (absent on a first install), `updater.target_version` and
`updater.previous_version` (absent on a first install) and `updater.artifact_digest` (text,
`sha256:` and 64 lowercase hex digits, the digest of the artifact the updater verified; present on
every step). The digest is an identity the hook keys its record on; it is not for verifying the
artifact again. `Params::artifact_digest` refuses any other shape. The manifest's own `[params]`
may not set any `updater.*` key (`Params::parse_declared` refuses it, and so does the manifest
loader).
**A hook still validates every value it reads from this file against its own record directory and
the manifest it shipped with**: a value read from a file is untrusted input.

`updater.previous_release` and `updater.previous_version` go together: a first install has neither,
an update has both (`Params::parse` and `Params::validate` refuse one without the other). The
release paths are absolute (a relative path is refused). A hook MAY refuse an unknown key of its own
`[params]`, but it MUST ignore an `updater.*` key it does not know, so the updater can add
reserved keys without breaking an older hook.

The updater writes **one params file per update** and passes that same file, with the same
values, to every step (`prepare`, `cutover`, `verify`, `rollback`, `cleanup`), so
`updater.staged_release` and `updater.target_version` are present on every step. The argument of
`prepare` is **byte for byte** the string in `updater.staged_release`; a hook may compare them
exactly, with no canonicalisation. The conformance harness gives every step one such file.

**Exit codes.** `0` success; `1` failure; `75` deferred (the host is busy, nothing irreversible
happened, try later); `76` halt (no version can repair this). Any other code, a timeout and a signal
are failures.

**The last line.** The last line a step writes to standard output is one JSON object:

```json
{"class":"release","reason":"drain_bound","retry_after_secs":300,"reversible":true,"promotion_pending":false,"detail":"text for the log"}
```

- `class`: `release` or `environment`. A step that dies without the line is `release`.
- `reason`: optional; a token of `[a-z0-9_]`, 1 to 64 bytes, for machines. Prose goes in `detail`.
- `retry_after_secs`: used with `75`.
- Deferral reasons a hook gives with `75` include `drain_bound` (a drain did not finish in its
  bound) and `leases_live` (the host holds live leases it will not cut; the hook decides it in
  `prepare`, after undoing what it placed).
- `reversible`: whether the hook can still undo what it has done; absent means `true`. A step that
  says `false` is the commit.
- `promotion_pending`: set by `verify` when the service serves but is not yet fully its own.

The updater reads the line field by field. A missing line, or one that is not a JSON object, is
`{}`. In an object, each invalid field (a `reason` that is not a token, a wrongly typed value, an
unknown `class`) takes its default and is reported as a warning, and the valid fields are kept; a
`reversible` that is present but not a boolean reads as `false`, because a step that might have
committed is treated as committed (a `null` too, on the strict reader as well). The strict reader and the harness refuse any invalid field and
a missing line.

**Every step is safe to run twice**, acts on the hook's own durable record rather than on the order it
was called in, and takes an exclusive `flock` on its record directory for the whole step (a second
copy exits `1` and changes nothing). `cleanup` refuses to run unless the record says `verify` succeeded.
A `cleanup` that fails is retried and then Failed(after_commit), unless its line states
`"reversible": true`: then nothing has committed, the updater runs `rollback`, and the host's result
is the rollback's: it reports Failed(before_commit) with the cleanup's class, and that is not
counted toward `rollback_after_failed`. A line that states `false`, or leaves `reversible` out
(an old hook), is never rolled back; `Report::reversible_stated` tells the two apart from a stated `true`.

**What runs next** is `hostkeeper_hook::next`, a pure function of the step, how it ended and a small
`Context`; the tests pin every row of the table in the design document.

## The harness

The conformance suite runs in the service's own CI. At runtime nothing confines a hook: the
harness is how a hook is shown to keep the contract before it ships.

How a service repository outside this workspace depends on it, and what it does not check: [docs/conformance.md](../../docs/conformance.md).

`hostkeeper_hook::conformance` is a library for a service's own tests (a dev-dependency). It is given the
hook's path and `hook_args` and a `FakeService`: the world the hook acts on, in a directory. It starts
the hook as the updater would and checks:

- every run ends with an exit code of the contract and a well-formed last line;
- each step, and `rollback`, run twice: the second run is a no-op;
- no run writes the staged release or the service's own data directory;
- a `rollback` that says it was reversible restores the world and leaves nothing behind; `cleanup`
  after a rollback changes nothing;
- the hook, stopped at every kill point its fake reaches (`conformance::gate::point`) and run again,
  and each step run again after it finished, reaches the world an uninterrupted run reaches;
- the updater's table driven through success and through a failure, a deferral and a halt at each
  step;
- the record is compared as well as the world, in reruns and after a resume; a killed `rollback`
  resumes like the other steps; a step that changes things with no kill point is refused;
- the params file and the sandbox root are guarded like the staged release;
- the same checks run as an update (previous release and version present, the release a real tree)
  and as a first install (neither); a params file with an unknown `updater.*` key is accepted;
- with the record directory locked, every step refuses.

**Allowances.** The hook's author may declare to the harness, with `Conformance::with_allowances`
and an `Allowances` in the hook's own test, up to three behaviours the checks above refuse by
default and that a hook may show for safety. Each is off unless declared, so a hook that declares
none is checked strictly. Nothing on the host reads these declarations:

- `rollback_rerun_may_say_reversible`: a `rollback` run again may say `reversible: true` where the
  first run said `false` (a rerun that finds no record cannot know the first committed; the updater
  ignores `reversible` on a `rollback` that succeeded). The rerun must still end as the first did
  and change nothing; a flag that falls, or a flip on another step, is still refused.
- `lapsing_cordon`: a `LapsingCordon`, the state paths of a cordon and the paths whose presence
  means the hook has recorded it as its own. After a crash right after placing the cordon and before
  recording it, a run again cannot tell it from an operator's and may leave it to lapse by its lease.
  Only the kill point right after the change that made a cordon path appear is excused, only while no
  ownership path is there (an empty ownership list excuses nothing), and only for the cordon paths,
  added or changed; a cordon left by a crash at any other point, a renewal included, is still
  refused. The kill point before a kill ran in another sandbox, so the window reads only which paths
  exist, never their bytes, which may name the sandbox.
- `first_install_rollback_may_fail`: on a first install, a `rollback` after `cutover` may fail
  (exit `1`; not a deferral or a halt) with the reason `no_previous_release` rather than stop what
  it started, changing nothing, and the same way, with the same reason and the same `reversible`,
  when run again. With a previous release, or with another reason, the rollback must still
  succeed. It covers the first-install `rollback` of `idempotence`, which `check_all` runs after its
  update checks; with `with_first_install`, crash-resume and the sequences still require the
  rollback to succeed.

The tests of this crate run the harness against a small example hook
(`tests/bin/example_hook.rs`) and against copies with one defect each, and require it to name each.
`tests/allowances.rs` runs it against the example with each allowed behaviour: refused when not
declared, accepted when declared, and refused, declared or not, just past what the allowance covers.
