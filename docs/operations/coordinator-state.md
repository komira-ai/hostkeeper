# The coordinator's state: backing it up and restoring it

The coordinator keeps its durable state in files named by three flags of `serve`: `--state-file`,
`--ops-file` (by default the state file's name with `.ops` added) and, when it is the hosts' CA,
`--enrollment-dir`. `hostkeeper-coordinator backup` copies all of it into a snapshot directory, and
`hostkeeper-coordinator restore` puts a snapshot back. Both take the same flags `serve` names the
state by. The CA's key is not part of the state and never in a snapshot: back it up once, sealed and
offline, as [ca-backup.md](ca-backup.md) says.

## What each file holds

| file | holds | without it |
|---|---|---|
| `--state-file` | The desired state of each service (version, limits, minimums) and the package digest of every version CI has set. | No service has a desired version until CI sets it again. |
| `<state-file>.jti` | The ids of CI's OIDC tokens already spent, each with the second until which it is refused. | A CI token spent before the loss can be used again until it expires. |
| `<state-file>.verdicts` | Per service and host, the last release verdict the coordinator acknowledged, and the verdicts it counts toward an automatic rollback. | A host whose daemon stopped resending its verdict is no longer read as having failed it, so it can be ordered that version again; the count toward an automatic rollback starts again. |
| `<state-file>.lock` | Nothing: the lock `serve` holds while it runs, so a second coordinator on the same files is refused. | Nothing. It is not backed up. |
| `--ops-file` | What operators changed: exclusions, labels, halts and their clears, holds, assignments, minimum versions, uninstall intents, the host inventory, the marks of last good and bad versions, and the audit ring. | All of it. |
| `<ops-file>.audit` | Every change to the ops file, one JSON line each. | The audit history. |
| `<ops-file>.audit.<n>` | The sealed segments of the audit file, the older changes, one JSON line each ([holds-and-force.md](holds-and-force.md#the-audit-file)). Written once the audit file has been rotated. | The audit history before the newest rotation. |
| `<ops-file>.halts` | The halt reports, who raised each halt, the reports kept while a halt stood, each service's clear generation, and each service's update plan position (`rollouts`: the version it is for, the stages entered, the hosts recorded done, an operator's pause; [rollout-plans.md](rollout-plans.md#what-a-restart-keeps)). Written once there is a halt or a plan that went past its first stage or was paused. | The halt reports and the clear generations: a halted service halts again after its clear. The plan positions: each plan starts at its first stage with no host recorded done, and an operator's pause is gone, so a paused plan goes on until it is paused again. |
| `<ops-file>.halts.next` | Only after a crash in the middle of a change: the halts file that change staged. The next start settles it. | The ops file holds that change and the halts file does not: what the change added to the halts file (a clear's generation, the reports of its acknowledgements) is lost. |
| `<ops-file>.halts.unfinished` | Only while a change that raises protection (a halt, a bad mark, a pause) or an operator's abort is in memory and not yet written, and after a crash in that window: the marker, listing the audit number of each such change (`{"unwritten":[41,42]}`). A start removes it when the ops file holds every change it lists; otherwise it refuses to start, naming the changes that did not land, until the operator makes them again by hand if they still matter and removes the marker. A marker the release before wrote (an empty file) refuses a start as it did. | A change it lists that did not land may be lost without notice. |
| `--enrollment-dir` | The enrollment store ([enrollment-coordinator.md](../enrollment-coordinator.md#state-files)): the requests, the certificates issued, the approved hosts (host id, key hash, role, services), the deny rows, the Mac nonces, the next certificate serial, and, with the MDM gate, its record of each device (`mdm-devices/`). | No enrolled host's heartbeat or renewal is admitted. Each host has to enroll again and be approved again. Hosts that were denied are no longer denied. |

## Backing up

A backup copies the files of a stopped coordinator. It takes the lock `serve` holds, so with the
coordinator running it is refused (`a coordinator holds the lock of these state files`) and nothing
is written. Stop the coordinator, back up, and start it again:

```
hostkeeper-coordinator backup --state-file <file> [--ops-file <file>] [--enrollment-dir <dir>] \
    --out <new directory>
```

The flags are the ones `serve` runs with. Leave out `--ops-file` if `serve` runs without it. Give
`--enrollment-dir` whenever `serve` has it, or the enrolled hosts are not in the snapshot.

- The backup refuses an `--out` that is already there. It also refuses an `<out>.partial`, which is
  where the snapshot is written before it is renamed to `--out`. A backup that fails after making
  its `<out>.partial` removes it.
- It refuses when none of the state files is there, or the state file's directory is not, which
  means a flag names the wrong file. It names each file of the state that is not there and so
  not in the backup: a file that should be there and is named means a flag is wrong.
- It copies every file listed above that is there, except the lock. That is each sealed segment
  of the audit file, too: the manifest names segment `<n>` as `ops-audit.<n>`, with the number it
  has. From the enrollment directory it copies each file ending `.pb`, in that directory and the
  directories under it.
- It copies `<ops-file>.halts.next` and `<ops-file>.halts.unfinished` as they are, so a restored
  coordinator starts exactly as the old one would have. The next start settles a staged halts
  file. A marker refuses the start unless the ops file holds every change it lists.
- Anything else in the enrollment directory is not copied: a `.tmp` file a crash left, or a key or
  certificate file kept there. The backup names each such file it passes over, except the
  store's `lock`.
- The snapshot holds `manifest.json`, which names each file with its size and SHA-256. The
  directory and its files are owner-only (0700 and 0600).
- The output lists each file copied, with its size and SHA-256. Its last line says the CA's key
  and certificate are not in the backup.

Copy the snapshot off the machine yourself; the backup writes only to `--out`. With the release's
container image, run the same image with the coordinator's volume mounted. The image's entrypoint
is the binary, so `backup ...` is the container's arguments, as `serve ...` is.

## Restoring after a lost disk

1. Put the CA's key and its `.cert` back from the sealed copy, at the path `--ca-key-file` names
   ([ca-backup.md](ca-backup.md)). The server certificate file need not be restored: the start
   makes it when it is missing.
2. Restore the snapshot to the paths `serve` will run with:

   ```
   hostkeeper-coordinator restore --from <snapshot> --state-file <file> [--ops-file <file>] \
       [--enrollment-dir <dir>] --into-empty
   ```

3. Start `serve` with the same flags.

What the restore checks and refuses:

- `--into-empty` is required. The restore writes only into targets that hold no state.
- It reads the manifest and every file of the snapshot first. It refuses the snapshot, writing
  nothing, when:
  - a file's size or SHA-256 is not what the manifest says;
  - the snapshot holds a file the manifest does not name, or lacks one it names;
  - the manifest names a path that is no file of a snapshot (a segment's name is `ops-audit.`
    and a number with no leading zero, 1 or more);
  - the snapshot holds the enrollment store and `--enrollment-dir` is not given.
- It then takes the lock. It refuses while a coordinator holds it.
- It refuses when any target file is already there, a sealed segment of the audit file among
  them (any `<ops-file>.audit.<n>`, even when it is the only file there), or when the enrollment
  directory has anything in it. An enrollment directory that is there and empty, a freshly mounted volume for
  example, is fine.
- It makes the missing directories, owner-only. It writes each file to `<file>.restoring` and links
  it into place, which refuses a name that is already there, so no file is written over. Each
  sealed segment of the audit file is written back under its number, so `Audit.List` reads the
  restored audit as it read before the backup.
- Before its first file it writes `<state-file>.restore-unfinished`, and it removes the marker
  after its last. A restore that stopped partway (killed, or the machine lost) leaves the marker:
  `serve` refuses to start while it is there, and so does a second restore. Remove the files the
  restore wrote and the marker, and restore again.
- A write that fails removes what the restore had written. The error names anything it could not
  remove; remove that before restoring again.

## Moving the coordinator to a new machine

1. Stop the coordinator on the old machine. Do not start it there again: the lock keeps a second
   coordinator off the same files on one machine only.
2. Back up there, and copy the snapshot to the new machine.
3. Copy the configuration `serve` reads and the backup does not: the rollout file, the OIDC
   allow-list, the tokens file or operator token file, the TLS files of `--tls-cert`, `--tls-key`
   and `--client-ca`, the Mac serial allowlist if any, the UI directory if any, and the CA's key
   and `.cert`.
4. Restore on the new machine, into the paths its `serve` will run with. They need not be the old
   paths: a snapshot names its files by what they are, not by where they were.
5. Point the names of `--server-name` at the new machine, and start `serve` there.

## Hosts enrolled after the last backup

A restore brings back the enrollment store as it was at the backup. Anything that changed after
the backup is lost, and every other file of the state is likewise as it was at the backup:

- A host approved after the backup has no host row in the restored store. Its heartbeats are
  refused even though its certificate is of the same CA, and it cannot renew. It has to enroll
  again: a new request, approved by an operator. If the request claims a machine id that another
  request in the store has, or a MAC bound to an enrolled host, it is a `duplicate_claim`, and the
  approval has to set `replace` (`Enrollment.Approve`,
  [enrollment-coordinator.md](../enrollment-coordinator.md)).
- A host whose key and host id are in the restored store is admitted as before.
- A host denied or retired after the backup is not denied in the restored store. Deny it again.
- The serial counter is the backup's. Certificates issued after the restore can take serial
  numbers that certificates issued after the backup already had.
- Desired versions CI set after the backup are lost; CI sets them again. Operator changes after
  the backup (holds, assignments, exclusions, halts) are lost; make them again, using the audit
  file of the lost machine if it survived. Ids of CI tokens spent after the backup are not
  refused until those tokens expire.

So back up after each change you would not want to make again, and before any move.
