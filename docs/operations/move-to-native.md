# The move to native packages

This runbook moves the farm from hostkeeper's own archives (`hostkeeper-archive`) to native
packages: `.deb` from an apt repository on Linux, a signed `.pkg` on a Mac. It runs stages 5 to 10
of [packages-native.md](../design/packages-native.md) section 10.6. For each stage it gives when
the stage may start, the query that shows it, the work, and how to back it out. Stage 6 puts the
repository in the coordinator's registry once, and checks that each host's daemon announces
`apt`. The other stages are releases, set by their own pull requests, and the queries that say
when the next may start.

A stage starts only when the previous one holds on every host. A service moves only once its own
release lists a native entry, so neither this runbook nor host setup moves a service early: host
setup only decides which hosts can move.

## What you need

- `hkctl`, the operator's command line, with the coordinator's private listener, its CA file and
  an operator token, as in [enrollment.md](enrollment.md). Every `hkctl` call below takes the same
  three global flags.
- `jq` 1.6 or later on the same machine. `hkctl` prints the coordinator's JSON
  ([gateway.md](../gateway.md)); each query below reads that JSON with one `jq` program.
- Root on each host, for stage 6.
- The version of two releases of `hostkeeper`, read from git:
  - **A4's release**: the first release with the daemon that takes a release's picked entry
    (packages A4);
  - **A19's release**: the first release with the daemon that updates itself through a native
    installer and moves a host (packages-native A19).

  The first release with a change is the oldest tag that contains its merge commit:
  `git tag --contains <merge-commit> --sort=v:refname`, first line, without its `v`.

## The queries

Each query is one `hkctl` call that writes the coordinator's answer to a file, and one `jq`
program that reads the file. A fleet query prints the number of hosts first, then one line per
host it counts: the node id, a colon, and why. A count of `0` prints that line alone.

The fleet queries read every host the coordinator lists: every host heard from since the
coordinator started, and every enrolled host that is not denied, heard or not. A host not heard
since the coordinator started is listed with what its enrollment kept of its daemon and of what it
runs. A query reads one page of 256 hosts. If the coordinator has more, the program stops with an
error instead of counting part of the fleet.

### Hosts whose daemon is older than a release

```
# query: older-daemons
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts list --limit 256 --response-file hosts.json
jq -r --arg floor <version> 'def triple: split(".") | if length == 3 and all(.[]; length > 0 and (explode | all(.[]; . >= 48 and . <= 57))) then map(tonumber) else null end; ($floor | triple) as $f | if $f == null then error("--arg floor is not major.minor.patch: \($floor)") elif (.nextPageToken // "") != "" then error("more hosts than one page: this query reads one page") else [.hosts[]? | (.heartbeat.agent // {}) as $a | (($a.version // "") | triple) as $v | if ($a | .name // "") != "hostkeeper" then "\(.nodeId): no hostkeeper daemon known" elif $v == null then "\(.nodeId): daemon \($a.version // "") is not major.minor.patch" elif $v < $f then "\(.nodeId): daemon \($a.version)" else empty end] | length, .[] end' hosts.json
```

`<version>` is the release, as `major.minor.patch`. A host is counted when:

- its daemon's version is below `<version>`, compared as three numbers (`0.1.99` is below
  `0.1.200`);
- its daemon's version is not `major.minor.patch`;
- no hostkeeper daemon is known: the host has sent no heartbeat that named its agent, or its agent
  is not hostkeeper (kbf's `kbf-updater` names none).

### Hosts that run a service from archives

```
# query: archive-hosts
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts list --limit 256 --response-file hosts.json
jq -r 'if (.nextPageToken // "") != "" then error("more hosts than one page: this query reads one page") else [.hosts[]? | {id: .nodeId, why: (if has("runningUnix") then [(.running // [])[] | select((.installer // "") == "hostkeeper-archive" or (.installer // "") == "") | "\(.service) \(.version // "") from \(if (.installer // "") == "" then "an installer not known" else .installer end)"] else ["nothing known of what it runs"] end)} | select(.why | length > 0)] | length, (.[] | "\(.id): \(.why | join(", "))") end' hosts.json
```

A host is counted once, whatever number of services it runs from archives, and its line names
each of them. A host is counted when it runs a service:

- from `hostkeeper-archive`. A daemon that does not announce `target-v1` runs every service from
  archives, and the coordinator says so;
- from an installer that is not known: a `target-v1` daemon whose installers the coordinator has
  not read over gRPC since it started, with nothing kept of the host;

and when nothing is known of what it runs (`running_unix` is absent), since it may run archives.
A host that runs every service natively, or runs none, is not counted.

### Hosts that run one service other than natively

```
# query: service-not-native
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts list --limit 256 --response-file hosts.json
jq -r --arg service <service> 'if (.nextPageToken // "") != "" then error("more hosts than one page: this query reads one page") else [.hosts[]? | {id: .nodeId, why: (if has("runningUnix") then [(.running // [])[] | select(.service == $service and ((.installer // "") == "hostkeeper-archive" or (.installer // "") == "")) | "\(.service) \(.version // "") from \(if (.installer // "") == "" then "an installer not known" else .installer end)"] else ["nothing known of what it runs"] end)} | select(.why | length > 0)] | length, (.[] | "\(.id): \(.why | join(", "))") end' hosts.json
```

The previous query for `<service>` alone: a host is counted when it runs `<service>` from
`hostkeeper-archive` or from an installer not known, or when nothing is known of what it runs.
A host that does not run `<service>` is not counted.

### Hosts whose daemon does not announce an installer

```
# query: not-announcing
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts list --limit 256 --response-file hosts.json
jq -r --arg installer apt 'if (.nextPageToken // "") != "" then error("more hosts than one page: this query reads one page") else [.hosts[]? | (.installers // []) as $l | select(any($l[]; . == $installer) | not) | "\(.nodeId): \(if $l == [] then "no installers read since the coordinator started" else "announces \($l | join(", "))" end)"] | length, .[] end' hosts.json
```

`--arg installer` is `apt` for a Linux host and `macos-pkg` for a Mac. The coordinator reads a
daemon's installers from its heartbeats over gRPC. Until it has read one since it started, the
host is counted with `no installers read since the coordinator started`, which is also how a host
not heard since then is shown.

### One host: whether its daemon announces an installer

```
# check: announces
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts get --node-id <id> --response-file host.json
jq -e --arg installer apt 'any(.host.installers[]?; . == $installer)' host.json
```

It prints `true` (exit 0) when the host's daemon announced the installer in its last heartbeat
over gRPC, and `false` (exit 1) otherwise. `hosts get` refuses `unknown_host` for a host not heard
since the coordinator started.

### One host: the updates it runs

```
# check: busy
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts get --node-id <id> --response-file host.json
jq -r '[.host.services[]? | select(.holdsSlot == true) | .service] | length, .[]' host.json
```

It prints the number of services of which the host holds an update slot, then each service: one
it is updating, whose promotion is pending, that failed and has not yet beaten healthy, or that it
was ordered and has not yet reported. `0` means that no update of the coordinator's runs there.

## Stage 5: daemons that can move

**Starts when** A4's release runs on every host: `older-daemons` with `--arg floor` A4's release
prints `0`.

**The work.** The releases of the daemon with the native installers (packages A6 and A7, and
`macos-pkg`, A9, on a Mac), the move and the read of stage 9 (packages-native A18 and A19), and the
native packages (A20) roll out as every release of `hostkeeper` does. Every release still lists
archive entries alone, and a daemon older than `target-v1` still fetches through the channel.

**Holds when** A19's release runs on every host: `older-daemons` with `--arg floor` A19's release
prints `0`. A host it names is brought up to that release, or removed from the fleet and
installed afresh ([network-install.md](network-install.md)).

**Back out**: set the previous release of `hostkeeper`. Nothing has moved.

## Stage 6: repository entries on each host

**Starts when** stage 5 holds on every host.

**The work.** Once, on the coordinator, then a check per host:

1. **Put the repository** `komira` in the coordinator's registry, with the suites `testing` and
   `stable`, the component `main`, the keyring and its fingerprints: `hkctl repositories put
   --request komira.json`, as [repository-keys.md](repository-keys.md#put-the-repository-in-the-registry)
   says. Every service of the rollout file names `komira` unless it names another repository;
   `hkctl repositories list` prints `unregistered` empty when the registry holds every one.
2. **Each host writes its entries.** At its next heartbeat, the daemon of each Linux host whose
   services name `komira` (from the release with `repositories-v1`) writes
   `/etc/hostkeeper/repositories/` from the registry: `komira-testing.sources`,
   `komira-stable.sources` and `komira.gpg` (and `komira-testing-fail.sources` while `komira` lists
   the suite `testing-fail` for hello's failing-build proof, [hello.md](../hello.md) step 4), checked with its own loader before it switches. A host
   that already holds those files byte for byte, as the held setup wrote them, keeps them. No
   restart: the daemon takes the entries and announces `apt` from the next heartbeat. `hkctl hosts
   get` with the node id shows `repositories` with `current` true; `refused` names the reason of a
   daemon that refused the set, which then keeps its directory as it was.
3. **Check that `apt` is announced.** After the host's next heartbeat, `announces` with its node
   id and `--arg installer apt` prints `true`. A host that prints `false` with its daemon
   running is one of:
   - not a `deb` host, or one whose apt tools are missing, or whose installed `apt` is older than
     2.2: it would stay on archives, and every later stage waits for it. Bring it up (the apt
     tools, `apt` 2.2 or later), or remove it from the fleet and install it afresh
     ([network-install.md](network-install.md));
   - a host whose daemon is older than A19's release: stage 5 does not hold on it;
   - a host whose daemon predates `repositories-v1`: its repositories show no `generation`. It
     takes the set once it runs the release that has it;
   - a host whose last heartbeat came over the JSON route: wait for the next one.

For the hosts installed from the network from now on, the boot server's `repositories` setting is
needed only by a host that must install from the repository before it reaches the coordinator
([network-install.md](network-install.md), step 3a); every other host gets the entries at its
first heartbeat.

A Mac takes the stores and the signers in place of the entries: through the coordinator's MDM
files, or by hand ([repository-keys.md](repository-keys.md)). Its daemon reads them at its start
(`launchctl kickstart -k system/dev.hostkeeper.hostkeeper`), and the check is `announces` with
`--arg installer macos-pkg`. No host of the farm is a Mac today.

**Holds when** `not-announcing` with `--arg installer apt` prints `0`. A host it still names
keeps stages 7 to 9 from holding, since it runs its services from archives.

**Back out**, while no host runs a service from `apt`: name another repository in each service of
the rollout file, or remove the services' entries that need `komira`, then `hkctl repositories
remove --name komira`. A host keeps the files it holds; to stop one announcing `apt`, as root,
remove the directory's files and the directory and restart the daemon, and the next release of
each service picks it the archive entry. A host that runs a service from `apt` backs out stage 7
or 8 first.

## Stage 7: `hello`, the canary

**Starts when** stage 6 holds on at least one host: `announces` prints `true` for it.

**The work.** `hello`'s release lists its native entry before its archive one (A21), and it is
set as every release of `hello` is. The rollout plan of `hello` moves its hosts: a host that runs
`hello` from archives and announces `apt` is picked the native entry, and installs the `.deb`
with its hook. A host that does not announce `apt` is picked the archive entry, as before.
Follow it with `hkctl releases converge --service hello`, and with `service-not-native` and
`--arg service hello`, which names the hosts still on archives.

**Holds when** `service-not-native` with `--arg service hello` prints `0`.

**Back out**: set `hello` again to a version lower than the one the moved hosts run, whose release
has no native entry: the version before stage 7, with its release as it was set
(`hkctl releases get-release --service hello --version <previous>` gives its targets). Or force
that version (`hkctl releases force`, [holds-and-force.md](holds-and-force.md#force)). A host
that moved is then picked the archive entry and rolled back to the archive version it kept as
`previous`, from its sealed copy, with nothing fetched. A version that is not lower is no back-out:
a host that moved is picked nothing for it (`installer_dropped`) and stays on its `.deb`. The
automatic rollback of a failed move does the same on one host. Once the next update of `hello`
on a host has retired that archive version, the move there cannot be undone.

## Stage 8: `mdm`, then `hostkeeper`

**Starts when** `hello` moved on every host: `service-not-native` with `--arg service hello`
prints `0`.

**The work.** `mdm`'s release lists its native entry first (A21), and then `hostkeeper`'s does
(A23), and each moves as `hello` did. `hostkeeper` moves by its self-update: the new daemon
confirms that it starts, then installs its own `.deb`, takes the archive's unit away from
`/etc/systemd/system/` and reloads systemd, and is not restarted. Follow each with
`service-not-native` and `--arg service mdm`, then `--arg service hostkeeper`.

**Holds when** both print `0`.

**Back out**: the same as stage 7, for each service, but `hostkeeper` is never forced (a force
of it is refused `self_service`): set its lower version. A rollback of `hostkeeper` to a release
older than A19's on a host where `hostkeeper` moved is refused `rollback_below_installer`, before
anything switches.

## Stage 9: archives stop

**Starts when** both counts of stage 9 are `0`, over every host the coordinator holds, whether or
not it heartbeats:

1. `archive-hosts` prints `0`: no host runs any service from archives;
2. `older-daemons` with `--arg floor` A19's release prints `0`: no host's daemon is older than
   A19's release, a daemon older than `target-v1` among them.

`releases converge` cannot give them: it counts one service's version, and a host picked nothing
does not hold its convergence. A host either query names is brought up, or removed from the fleet
and installed afresh ([network-install.md](network-install.md)), so that no host is left with a
daemon that nothing can update. The pull request of stage 9 (A24) shows both queries' output.

**The work.** Every release is set with native entries alone (A24): per Linux platform the
`.deb`'s `apt` entry, and for macOS the signed `.pkg`'s `macos-pkg` entry alone, which a Mac whose
daemon announces `macos-pkg` takes (a Mac that does not is ordered nothing: `installer_dropped` or
`no_build_for_host`). `release.yml` no longer waits for the channel. Once A24's release has
converged on every host, stop the channel's sync, remove `--release-channel` from the
coordinator's deployment, and delete the channel's tree (packages-native.md section 10.5).

**Back out**: set a release with archive entries again; the code is still there (the
`cargo xtask release targets` of a commit before A24 writes them, with the release's index).

## Stage 10: deletion

**Starts when** stage 9's release has converged on every host: `hkctl releases converge` with
each service's `--service` says so.

**The work.** The deletion in the daemon, the coordinator, the crates, xtask, the workflows and
the docs (A25 and A28).

**Back out**: a rollback of `hostkeeper` to a release older than A25's is allowed: the releases
from A19's to A24's read native versions.

After stage 9, a host set up from a `.deb` or `.pkg` older than A19's release has a daemon that
can install nothing a release lists: it shows `no_build_for_host` for `hostkeeper`. Set it up
again from the current package.
