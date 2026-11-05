# macOS

What hostkeeper does differently on a Mac (design section 5.9). The update flow is the same as on
Linux: an order, the five steps of the hook, a step record, a verdict. This page says what the
code in this repository does on a Mac today, and lists what the design specifies that is not
built yet.

## Built

### Package verification

On a Mac, the unpack of a package (`hostkeeper-unpack`, `unpack` with `Platform::MacOs`) adds
two checks to the archive rules, after the archive's SHA-256 is checked and before the manifest
is read or the tree is sealed:

- no entry of the unpacked tree, file or directory, carries a `com.apple.quarantine`
  extended attribute. The archive rules already refuse every extended attribute in the tar
  bytes; this check is on the tree on disk, because macOS can add the attribute when a tool
  writes files, whatever the archive held;
- every Mach-O file of the tree (found by its magic number) has a valid code signature of its
  own bytes, as `codesign --verify --strict` judges it: ad-hoc or any identity. It is not a
  check of a team identity or of notarisation; which signers the daemon trusts is the
  package's attestation (design section 5.5, step 2). Hook scripts and other files that are
  not Mach-O have no signature and are not checked.

A quarantined entry, an unsigned binary or an invalid signature fails the order: `Failed`, class
`release`, reason `bad_archive`, with the relative path in the detail. A check that cannot be
made (the tool could not run, was killed or did not finish) is `Deferred`, class
`environment`, reason `verify_unavailable`: the package is not judged bad. In every failing
case the staging directory is removed and nothing is sealed or published. On Linux neither check
runs. [package-format.md](package-format.md) has the rule as part of the package format.

### Live leases: `leases_live`

A Mac that holds live leases defers an update rather than cut them. The service's own hook
decides it, in `prepare`, after undoing anything it placed: it exits `75` with the line

```json
{"class":"environment","reason":"leases_live","retry_after_secs":60}
```

The daemon never decides `leases_live` itself. It reads the exit like any other `75` from
`prepare`: no cutover is attempted, the hook's `rollback` runs as after any deferred `prepare`,
and the order ends `Deferred` with the hook's reason and retry time.
The token is `hostkeeper_hook::REASON_LEASES_LIVE`, beside `drain_bound`.

### Self-update fallback

On Linux the daemon's unit restarts it, and an `OnFailure=` unit runs the previous binary's
`self-update-fallback` verb once the new binary has failed its start limit. launchd has neither
a start limit nor `OnFailure=`, so on a Mac the same verb is run by a timer job.
`hostkeeper_steps::selfupdate` returns the text of both plists:

- `launchd_main_plist`: the daemon's `LaunchDaemons` plist, running `current` at load with
  `KeepAlive` set to `true` (the counterpart of the unit's `Restart=always`: the daemon exits
  cleanly after switching to a new binary and must be started again) and a `ThrottleInterval`;
- `launchd_fallback_job`: the job `<label>.fallback`, which runs
  `previous self-update-fallback --dir <dir>` every `StartInterval` seconds and not at load.

The plists are deterministic (keys sorted, fixed indentation) and every path and label in them
is XML-escaped.

### Enrollment claims

A Mac's enrollment request claims its serial number (`IOPlatformSerialNumber`) and its platform
UUID (`IOPlatformUUID`), in the `mac_serial` and `platform_uuid` fields of the claims; the Linux
claims (machine id, DMI serial and product uuid, network addresses) are left empty.
`hostkeeper_enroll::MacClaims` reads them from the output of
`ioreg -rd1 -c IOPlatformExpertDevice` through a port, and `parse_ioreg` parses it. Unlike a
Linux claim, which is absent when it cannot be read, a missing or malformed Mac value is an error
that stops enrollment before any request is posted. No placeholder is ever sent.

## Specified, not built yet

- The launchd daemon `com.<reverse-dns>.hostkeeper`, the launchd job per hook step (timeout,
  kill and log capture only), the macOS paths and the placed plists in `/Library/LaunchDaemons`.
- The real adapters behind the ports above: the quarantine read (`listxattr`/`getxattr`),
  `codesign` run with a fixed argument list, and `ioreg`.
- The self-update verb's decision on a Mac: whether the new binary failed its burst of starts
  within the window, from the starts the daemon recorded.
- Process identity by `proc_pidpath` plus a device and inode check, and what the observer reports
  when the path was replaced while the process runs.
- The device run: the daemon under launchd on a GitHub-hosted macOS runner, in the workflow that
  runs on pushes to `main` and on dispatch, never on a pull request.
