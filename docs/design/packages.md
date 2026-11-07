# Packages in any format: targets per platform, installers, and a manifest outside the artifact

Status: design, for the maintainers' ruling. Nothing here is built. This is a revision of the design merged as
komira-ai/hostkeeper#179. It keeps that design's stores, its digest and attestation checks before any byte is used, and its
staged path for the daemons on hosts now. It replaces its package model: host facts, "most specific wins", artifact kinds
and `Releases.Register` (section 0 says what replaces what). It changes three things in
[hostkeeper-design.md](hostkeeper-design.md) section 5.5 ("One package per operating system and CPU family", "The release
channel", "The coordinator reads the index too") and the platform line of its section 4.7. Where those disagree with this
document on what a package is, how a release reaches the coordinator or where the daemon fetches bytes, this document wins
once it is built. The tags are those of hostkeeper-design.md: **Built** names code on `main` with a path, and **Designed**
means nothing implements it yet. Section 13 lists the build items, A1 to A12, one pull request each.

Today a service can be deployed by hostkeeper only as hostkeeper's own package: a `tar.zst` archive holding
`manifest.toml`, `bin/`, `units/` and `files/`. Every service must repackage what it builds into that shape, even when it
already ships a `.deb`, an `.rpm`, a macOS `.pkg` or an OCI image. This design removes hostkeeper's package format:

1. A release names, for each platform, one artifact in any format. hostkeeper fetches it by digest, verifies its
   digest and attestation, and never parses it. Only the format's own tool reads it, after the checks.
2. What hostkeeper needs to know about the service is a **manifest** sent in the same call, one per platform, beside the
   artifact. It is not inside the artifact.
3. The code that installs a format is an **installer**, built into the daemon, with the contract of a hook. A format that
   has no installer yet is shipped as a plain tarball whose own hook installs it, with no hostkeeper release.

## 0. What this revision replaces in the design of komira-ai/hostkeeper#179

| Section of #179 | Fate here |
|---|---|
| 1, 2. What changes; today, in the code | Rewritten (sections 1, 2) |
| 3, 4. Host facts; builds and "most specific wins" | Dropped from v1: a host gets its platform's target, or none (section 3.3). A short note in section 11 ("Later") |
| 5. Artifact kinds (`archive`, `oci-image` as a payload, "not in v1", "what no kind may do") | Replaced by installers (section 5). `.deb`, `.rpm` and `.pkg` are in v1; an OCI image is an artifact of its own |
| 6.1 to 6.3 `Releases.Register`, its checks, desired state over it | Replaced by one call, `SetDesired` with `targets`, and its checks (sections 3.1 to 3.4) |
| 6.4 Where the coordinator keeps a release | Kept (section 3.6), without the per-host build record, which a platform map makes unneeded |
| 6.5 CI publishes with the generated client | Kept, with the new call (section 3.7) |
| 7. Stores and the fetch | Kept (section 7), with the attestation found through the target |
| 8. Guarantees | Rewritten (section 8), with the unattested manifest stated |
| 9. Compatibility and the staged path | Rewritten (section 9) for targets and installers; same shape, same rule that old daemons only get archives |
| 10. Fit with two-programs and the hook contract | Rewritten (section 10): the hook contract gains installers and built-in steps |
| 11 to 14. Tests, build items P1 to P12, decisions, questions | Replaced by sections 12 to 15; section 13 maps each P item to its fate |

## 1. What changes for the operator and for a service's CI

**Before.**

- A service is shipped as one `tar.zst` archive per platform, in hostkeeper's layout, with `manifest.toml` inside.
- CI publishes a GitHub release. On the channel's host, `cargo xtask release channel-sync` pulls it into
  `<channel>/<service>/<version>/`, and CI sets the version desired with the release index inline
  (`hostkeeper ci set-desired --release-index-file`).
- Every daemon fetches from the one `release_channel` of its config, over HTTPS, without credentials.

**After.**

- CI deploys a version with one call, `Releases.SetDesired`, naming the service, the version and its **targets**: for
  each of the three platforms, an artifact (where it is, its digest and size, its attestation bundle) and that
  platform's manifest, or nothing.

  ```json
  {
    "service": "tierstore",
    "desired": { "version": "1.6.0" },
    "targets": {
      "linux-x86_64": { "locations": [...], "digest": "<64 hex>", "bytes": "48812032",
                        "attestation": { "digest": "<64 hex>", "bytes": "7311", "locations": [...] },
                        "manifest": { "installer": "deb", "package": "tierstore", ... } },
      "linux-arm64":  null,
      "macos-arm64":  { ..., "manifest": { "installer": "macos-pkg", "package": "dev.tierstore.server", ... } }
    }
  }
  ```

- A host is ordered the version only when its platform has a target. A host of a platform with none keeps what it runs
  and shows `no_target`. It is neither failed nor missing.
- The artifact is any format with an installer: a `.deb`, an `.rpm`, a macOS flat `.pkg`, an OCI image archive, or a
  plain tarball (a Homebrew bottle is one), as well as today's archive.
- The bytes stay where CI published them (the GitHub release's assets) and may also sit in a mirror the host is
  configured to use. A daemon reads its stores and their credentials from `/etc/hostkeeper/stores.toml`.
- The manifest, which names the root-run hook, comes from the call and is not attested. Whoever may set a service's
  release decides what runs as root on its hosts, within the attested bytes (section 8.2).

**What does not change.**

- An order names exact bytes by SHA-256. The daemon checks size, digest and attestation before any byte is read by
  anything.
- Nothing about a store, a location or the coordinator's choice makes bytes trusted.
- The hook contract (steps, exit codes, last line, params), the step record, placement of a tarball's units and files,
  health observation and the rollout gate. The contract gains two things (section 10): installer steps beside it, and
  built-in steps for a service with no hand-over of its own.
- The three platforms hostkeeper itself is built for, and its `compile_error!` on any other target.
- Daemons already on hosts keep working at every stage of section 9 with no change. They keep receiving archives.

## 2. Today, in the code

Every item is **Built** at the path given.

- **The package.** One `tar.zst` per service, version and platform, read by `crates/hostkeeper-unpack`: one zstd
  frame, ustar, regular files and directories only, top level `manifest.toml`, `bin/`, `units/`, `files/`, bounded by
  `max_unpacked_bytes` and `max_entries` (`docs/package-format.md`). Its `manifest.toml` (`Manifest::load`,
  `crates/hostkeeper-config/src/manifest.rs`) holds `schema = 1`, `name`, `version`, `platform`, `min_hostkeeper`,
  `[hook]`, `[units] run`, `[health] url`, `[[files]]` and `[params]`; every table but `[params]` refuses an unknown key.
- **The fetch and the proof.** `crates/hostkeeper-package/src/fetch.rs` takes its platform's entry of
  `{release_channel}/S/V/index.json` and downloads the archive, checking size and digest; `prove.rs` verifies
  `<archive>.sigstore.json` with the pinned `gh attestation verify` against `[trusted_signers]` in
  `/etc/hostkeeper/hostkeeper.toml`. Only then does `VerifiedArchive` exist, the one way to the bytes.
- **On the host.** `/var/lib/hostkeeper/packages/<service>/<version>/` (0555), `<version>.seal` (tree digest and package
  digest), `current` and `previous` links; two versions kept (`crates/hostkeeper-steps/src/prune.rs`). `Staged::check`
  re-hashes the tree before every hook step (`crates/hostkeeper-steps/src/artifact.rs`); the tree digest refuses
  anything but regular files and directories.
- **Placement.** The daemon links each file of `units/` and each `[[files]]` entry to its system path, pointing into
  `current`, and records every path in `/var/lib/hostkeeper/placed.json` (`crates/hostkeeper-placed`). Removal removes
  only a path the record lists and only while it is still the link recorded (`remove.rs`). That is what this design
  calls **closed-world removal**: hostkeeper removes what it put on the host, all of it, and nothing else.
- **The hook.** A program inside the package, named by `[hook] command`, run as root in a bounded transient unit
  (`crates/hostkeeper-steps/src/systemd.rs`): `prepare`, `cutover`, `verify`, `rollback`, `cleanup`, and the additive
  `uninstall` and `boot` (`crates/hostkeeper-hook`). Before `cutover` the daemon switches `current` and relinks
  (`hooked_switch`, `crates/hostkeeper-daemon/src/place.rs`).
- **The coordinator.** `Releases.SetDesired` takes `package_digest` (one package for every host) or `release_index` with
  `release_index_digest` (`operator.proto`). It orders each host the digest of its platform's entry and skips a host
  whose platform the index does not list, with `GATE_REASON_NO_PACKAGE_FOR_PLATFORM`; such hosts are not counted as
  missing (`Skip::NoPackageForPlatform`, `crates/hostkeeper-coordinator/src/decide.rs` and `membership.rs`). Every
  operator method is capped at 4 KiB (`MAX_OPERATOR_BYTES`, `crates/hostkeeper-coordinator/src/http.rs`). A heartbeat
  reply carries at most one order (`HeartbeatResponse.order = 1`), and the daemon reads a reply of at most 16 KiB
  (`REPLY_MAX`, `crates/hostkeeper-link/src/mtls.rs`).
- **The host's platform.** `HeartbeatRequest.platform` (values 1 to 3), from the daemon's compile target; `index_platform`
  (`crates/hostkeeper-coordinator/src/registry.rs`) maps it to `linux-x86_64`, `linux-arm64`, `macos-arm64`.
- **Features.** At most 16 in `HeartbeatRequest.features`, nine today (`crates/hostkeeper-link/src/beat.rs`); the
  coordinator echoes those it supports, and sends a verb only to a daemon that announced it.
- **Two routes and the audit.** The first heartbeat after start, and a gRPC one that failed in transport, go to the
  frozen JSON route (`crates/hostkeeper-link/src/negotiated.rs`). An operator's set is audited; CI's is only logged.

## 3. The deploy call: `SetDesired` with targets

### 3.1 The messages

The release messages live in a new file, `hostkeeper/release/v1/release.proto`, imported by `operator.proto` and
`agent.proto`, so the coordinator forwards to the host the target CI sent, field for field.

```proto
// release.proto (new)
message Target {
  repeated Location locations = 1;  // where the artifact's bytes are, in preference order; 1 to 8
  string digest = 2;                // SHA-256 of the artifact, 64 lower-case hex
  uint64 bytes = 3;                 // its size, 1 up to the host's max_package_bytes
  Blob attestation = 4;             // required: the artifact's Sigstore bundle, itself named by digest
  Manifest manifest = 5;            // required: section 4
}
message Blob { string digest = 1; uint64 bytes = 2; repeated Location locations = 3; }   // bytes 1 to 1 MiB; 1 to 8 locations
message Location { string store = 1; string locator = 2; }   // section 7.1

// operator.proto, additive
message SetDesiredRequest {
  // 1 to 7 as today
  map<string, Target> targets = 8;  // keyed by platform: "linux-x86_64", "linux-arm64", "macos-arm64"
}
service Releases {
  // today's methods
  rpc GetRelease(GetReleaseRequest) returns (GetReleaseResponse);
}
message GetReleaseRequest { string service = 1; string version = 2; }
message GetReleaseResponse { map<string, Target> targets = 1; optional string release_index_digest = 2; }
```

The requested shape names the service `package_name`. The call keeps today's field `service`, and the version stays in
`desired.version`, because renaming a shipped field is not additive (rule B8). `GetRelease` answers what was set, for a
mirror's sync job and for an operator. It takes the operator token or CI's OIDC token, as `Converge` does, and refuses
a service the coordinator does not manage with `UNKNOWN_SERVICE` (today's value) and a version never set with targets
with the new `RELEASE_UNKNOWN = 25`.

### 3.2 A map keyed by platform, not three fields

`targets` is a `map<string, Target>` keyed by the platform's spelling. The three keys are the documented set, and the
coordinator refuses any other key in v1 (`target_invalid`). Two reasons:

1. **The JSON is the shape asked for.** The proto3 JSON mapping writes a map as an object keyed by its strings, so the
   body is `"targets": {"linux-x86_64": {...}}`, keyed by the platform's own spelling. Three fields could only be named
   `linux_x86_64` or `linuxX8664`, never the spelling with a hyphen that the release index and channel use.
2. **A fourth platform is data in the operator API.** It needs a daemon built for it and a value in the agent's
   `Platform` enum, but no change to `operator.proto`, its generated clients or CI's calls. With three fields, every
   client would be regenerated for it.

A host's platform name comes from `HeartbeatRequest.platform`, mapped by `index_platform`. The daemon takes it from its
compile target, as today, so nothing the host could misreport chooses its artifact.

A platform with no target is left out of the map. The JSON parser the gateway uses refuses a `null` map value today, so
A2 adds one step before it: in a `targets` object, a key whose value is `null` is dropped. The requested shape with
explicit nulls is then accepted as written, and means the same as the key left out.

### 3.3 Which hosts are ordered

For a version set with targets, each host of the service is ordered the target of its platform:

- **Its platform has a target, and its daemon can install it** (section 9.1): it is ordered, exactly as a host is
  ordered today, through the update plan and the rollout gate.
- **Its platform has no target:** it is ordered nothing, keeps what it runs, and shows the gate reason
  `GATE_REASON_NO_TARGET`. `Releases.Status` and `Converge` list it apart, as they list `no_package_for_platform` hosts
  today. It is never counted as failed, and it does not hold convergence: the version has converged when every host
  with a target runs it.
- **Its platform has a target its daemon cannot install:** it is ordered nothing. It shows
  `GATE_REASON_TARGET_UNSUPPORTED` when the daemon does not announce `target-v1` and the target is not an archive it
  can fetch through the channel (section 9.1), and `GATE_REASON_INSTALLER_UNSUPPORTED` when the daemon announces
  `target-v1` but does not list the target's installer (section 5.1). It is listed apart too, not as failed, but
  `Converge` names it, because the release meant it to update.

One platform has one target, so the coordinator picks nothing and guesses nothing.

**Only over gRPC.** A target travels only in a reply over gRPC, and the coordinator takes a host's `installers` only
from a gRPC heartbeat, keeping the last list it got: the JSON route is frozen. When a host's heartbeat arrives over
JSON (the first after a start, or one resent after a gRPC transport failure), an order for an archive target goes out as
an old daemon's order (section 9.1), and an order for any other target waits for the next gRPC heartbeat. The host's
gate reason does not change for that one beat.

`Force` to a version set with targets uses the same rule. `ForceRollback` and automatic rollback go to the version the
host ran before. When that version is still on the host, the daemon uses its sealed `target.json` and needs no fetch
(section 6.4); the order still carries the target, for a host that no longer keeps the version.

### 3.4 What the coordinator checks

| Check | Refusal (`ReleasesRefusal`) |
|---|---|
| At least one target; each key one of the three platforms | `TARGET_INVALID = 23` |
| Every digest 64 lower-case hex; `bytes` from 1 up; an attestation of 1 byte to 1 MiB; 1 to 8 locations for the artifact and for the attestation, each of a store name and a locator of the grammar of section 7.1; at most 2 KiB of locators in all per target; at most 24 KiB per target encoded | `TARGET_INVALID` |
| The manifest of every target passes the checks of section 4.4 that need no host (all but the host's bounds and the files on disk) | `TARGET_INVALID` |
| The service `hostkeeper` with a target of another installer than `hostkeeper-archive` | `TARGET_INVALID` |
| `targets` together with `package_digest` | `INDEX_INVALID` (today's value for a set with both forms) |
| `targets` together with `release_index`: the index lists exactly the platforms whose target is `hostkeeper-archive`, each with that target's size and digest | `INDEX_INVALID`; `INDEX_DIGEST_MISMATCH` as today |
| The same service and version set before with other targets | `RELEASE_EXISTS = 24`, not retryable |

An index may list fewer than three platforms today (that is what `no_package_for_platform` is for), so a release with a
null platform carries an index too.

A release is immutable. Setting the same version again with the same targets, compared field for field, is accepted as
any repeated set is today. A changed artifact or manifest is a new version. The coordinator does not verify attestations
or fetch bytes. The daemon is the one verifier, as today, and a bad digest or bundle costs the first host a `Failed`
with class `release`.

`SetDesired` with `package_digest` or `release_index` and no targets keeps today's meaning and is never refused for
using them (rule B8).

Every `SetDesired` with targets is an audit entry, `set_desired`, whether CI or an operator made it, with the version,
the CI token's repository, workflow and ref or the operator token's name, and for each platform the artifact's digest
and the SHA-256 of the manifest's encoding. Today only an operator's set is audited (section 2). The manifest now
decides root commands (section 8.2), so who set which manifest must be on record.

### 3.5 Size

A target is at most 24 KiB encoded: a manifest of at most 16 KiB (section 4.4), at most 2 KiB of locators, two digests
and the rest. The message limit of `SetDesired` becomes 96 KiB, on the gRPC route and on the JSON gateway: three
targets at their largest, an inline index (a few hundred bytes for three platforms) and today's fields. Every other
operator method keeps 4 KiB.

An order carries one target, and a reply carries at most one order. A daemon that announces `target-v1` reads a
coordinator reply of up to 32 KiB, which holds one order at its largest with room for the rest of the reply. An older
daemon is never sent a target, so its 16 KiB limit holds.

### 3.6 Where the coordinator keeps a release

As in #179 section 6.4: a release is new state, kept beside the registry, not in it. `IndexRecord` and the state file
refuse unknown fields, so a new key in either would stop an older coordinator from starting after a rollback.

- With the file store, releases live in `<state-file>.releases`, each written once (temp file, `fsync`, rename).
- With the Postgres store, they live in a table `releases (service, version, targets jsonb, release_index_digest)`,
  added by its own migration marked `may-run` (coordinator-storage.md section 6).
- The registry's cap of 4096 versions per service, and its eviction, apply to releases too.

The registry and the desired state are written as for a set with neither a digest nor an index, today's third form:
the version and its limits. A coordinator rolled back to one before A2 starts on that state and does not read the
releases file or table. It shows the version `no_digest` and orders nothing, so no host is ordered bytes it cannot name.

The coordinator needs no per-host record of what it ordered: a host's target is always its platform's target of the
version. The previous version a rollback goes to is the one the host reports in its heartbeat, as today. A release
evicted with its version from the registry is evicted from the releases too, and the version may then be set again.

### 3.7 CI deploys with the generated client

The service's release workflow, after its GitHub release is published:

1. Builds the call from the release's assets and from its manifests, one per platform, kept in the service's
   repository next to the build that produces each artifact.
2. Calls `Releases.SetDesired` with the targets, through the generated bash client of the two-programs design
   (`docs/design/two-programs.md`), with a fresh OIDC token, as `set-desired.yml` does today.

A release whose targets are archives also carries its release index, so an old daemon is ordered it through the
channel: for every service until stage 5 (section 9.2), and for the service `hostkeeper` for as long as the channel
keeps its archives (section 15, question 3). Without an index, an old daemon is ordered an archive target with no index
digest, as a set with `package_digest` orders it today, and fetches it if the channel holds it. Nothing is uploaded except what the workflow already publishes. A LAN mirror fills
itself by pulling what `GetRelease` answers, never by a push from CI.

## 4. The manifest

### 4.1 One complete manifest per platform

Each target carries its own manifest, complete on its own. There is no manifest shared by platforms and no override
block. The Linux and macOS manifests of one service differ where the platforms differ (the installer, the package
identity, unit names against launchd labels, paths), and CI writes each from the service's own source. The manifest
is not inside the artifact and is not attested (section 8.2).

### 4.2 The message

```proto
// release.proto (new)
message Manifest {
  string installer = 1;               // "tar", "deb", "rpm", "macos-pkg", "oci-image", "hostkeeper-archive"
  optional string package = 2;        // the identity the format's own tool knows the package by
  Hook hook = 3;                      // absent only for hostkeeper-archive
  repeated string run = 4;            // the units (systemd) or labels (launchd) the service runs
  optional string health_url = 5;     // the loopback health URL, as [health] url today
  repeated Place place = 6;           // tar only: tree files the daemon links into the system
  map<string, Param> params = 7;      // opaque to hostkeeper, given to the hook
  string min_hostkeeper = 8;          // the oldest daemon that may install it
  repeated Container containers = 9;  // oci-image only: how to run the image
  InstallerBounds installer_bounds = 10;
}
message Hook {
  repeated string command = 1;        // a program in the staged tree and its leading arguments; empty: built-in steps
  optional string boot = 2; StepBounds timeout_secs = 3; uint64 verify_deadline_secs = 4;
  optional uint64 promotion_ceiling_secs = 5; optional uint32 cleanup_retries = 6;
}
message StepBounds { uint64 prepare = 1; uint64 cutover = 2; uint64 verify = 3; uint64 rollback = 4;
                     uint64 cleanup = 5; optional uint64 uninstall = 6; }
message InstallerBounds { uint64 stage_secs = 1; uint64 activate_secs = 2; uint64 remove_secs = 3; }
message Place { string kind = 1; string source = 2; string path = 3; }   // kind "unit" or "file"
message Param { oneof value { string text = 1; int64 integer = 2; double float = 3; bool flag = 4; } }
message Container { string unit = 1; repeated string args = 2; repeated Port ports = 3;
                    repeated Volume volumes = 4; }
message Port { uint32 host = 1; uint32 container = 2; bool loopback_only = 3; }
message Volume { string host_path = 1; string container_path = 2; bool read_only = 3; }
```

The service's name and version are the call's, not fields of the manifest, so they cannot disagree.

### 4.3 Why each field must be there

| Field | Why hostkeeper needs it |
|---|---|
| `installer` | The daemon never sniffs a format. It must be told which built-in code reads and installs the bytes, and the coordinator must know it to send the target only to a daemon that can install it (section 3.3) |
| `package` | The format's tool knows a package by a name (`dpkg`, `rpm`) or an identifier (`pkgutil`). The daemon checks the artifact is that package before it installs it, refuses to take over a package it did not install, and removes that package on uninstall (section 6.5). Required for `deb`, `rpm`, `macos-pkg`; refused for the others |
| `hook` | The hand-over from old to new is the service's (section 6.2). The bounds are what lets the daemon stop a step, and `verify_deadline_secs` is the verify window, as today. An empty `command` asks for the built-in steps (section 6.3) |
| `run` | The units the daemon stops before uninstall, starts and stops in `force` and the built-in steps, and observes for liveness and identity. Without it the daemon could not tell the service's units from any other unit a package installed |
| `health_url` | The daemon's observer probes it for the heartbeat and the bake gate, as today. Optional, as today |
| `place` | A tarball has no installer that puts its files into the system, so the daemon links them, as it links `units/` and `[[files]]` today. Native formats place their own files, so it is refused for them |
| `params` | The hook's opaque settings, as `[params]` today |
| `min_hostkeeper` | A manifest may use a field or installer a daemon does not know. A daemon older than this defers with `daemon_too_old`, as today, instead of misreading it |
| `containers` | An OCI image has no units. The `oci-image` installer writes one unit per entry from a fixed template (section 5.2). Required for `oci-image`, refused for the others |
| `installer_bounds` | The installer's steps run the format's own tools and scripts (a deb's `postinst`), which can hang. Each step needs a bound, as every hook step has one |

### 4.4 What the daemon checks before running anything

The manifest is checked twice: by the coordinator when the call is made (section 3.4), so CI learns of a mistake at once,
and by the daemon on receipt, before the fetch, because the daemon trusts no check the coordinator made. The daemon's
checks are today's `Manifest::load` rules moved to the message, plus those for installers:

- **Version.** `min_hostkeeper` is read first. A daemon older than it answers `Deferred`, `environment`,
  `daemon_too_old`, as today, whatever else the manifest holds; that is not a refusal of the manifest.
- **Shape.** The proto decodes, the installer is one this daemon lists, the fields its installer requires are present
  and those it refuses absent. At most 16 KiB encoded, 64 `run` entries, 256 `place` entries, 64 params, 32 containers.
- **Bounds.** As today: every step bound but `prepare` non-zero; `verify_deadline_secs` at least `verify` plus
  `rollback`; `promotion_ceiling_secs` not 0. Each installer bound from 1 up to 3600 seconds.
- **The hook program.** `command[0]` is a relative path of plain names, no `.` or `..`. It is relative to `tree/` of
  the version directory (section 6.1). After staging it must be a regular file there, reached through no link. A
  program outside the attested bytes can never be named. For `oci-image`, `command` must be empty, since an image
  stages no tree.
- **Names.** Each `run` entry and each container's `unit` is a unit of the service, by today's rules (`names.rs`): for
  systemd, a base of `<service>` or `<service>-…`, an optional `@<instance>`, and a suffix `.service`, `.socket` or
  `.timer`; for launchd, a label `<service>` or `<service>.…`. A unit `place` entry's `path` is such a name too. Every
  container's `unit` is also in `run`.
- **Paths.** A file `place` entry's `path` is absolute and outside `REFUSED_PREFIXES`, its `source` a relative path of
  the tree, as `[[files]]` today. A placed unit's exec lines may name package files only through `current/`
  (`lint.rs`). A container volume's `host_path` is under `/var/lib/<service>/`, with no `.` or `..` segment; its
  `container_path` is absolute. A container port's `host` is 1024 to 65535.
- **Health URL.** `http://` to `127.0.0.1` or `[::1]`, as today.
- **Params.** Scalars only, no `updater` or `updater.*` key.

A manifest that fails any check but the version is refused `Failed`, `release`, `manifest_invalid`, before any byte is
fetched. These checks are guards against mistakes. They are not what makes the hook safe to run; section 8.2 says what
does.

## 5. Installers

### 5.1 The contract

An **installer** is the code that turns a verified artifact into installed software, and takes it away again. It has
the contract of a hook (section 10): its steps are idempotent, bounded, run their commands in transient units, and end
in an outcome with a class and a reason. It has four steps:

| Step | Does | Must not |
|---|---|---|
| `stage` | Makes the version directory from the verified artifact: keeps the artifact, and extracts from it, with the format's own extractor, the tree the hook and the observer read. Checks the artifact is the package the manifest names | Change anything outside the version's staging directory, or run any script of the package |
| `activate` | Makes this version the installed one: installs it with the format's tool, then switches `current`. Is also how a rollback goes back (activating the kept previous version) | Fetch anything; start, stop or restart the service's `run` units (that is the hook's); touch another service's package |
| `retire` | Drops a version that is neither `current` nor `previous` (today's prune): deletes its directory, its seal and its kept artifact | Call the format's tool on the host's installed package, which belongs to `current` |
| `remove` | Takes the whole service off the host, on uninstall or when a first install fails: what its `activate` recorded | Remove anything `activate` did not record, or anything changed since |

Installers are **built into the daemon**. Each is a module of a new crate, `hostkeeper-install`, behind one trait, and
runs the format's tool by absolute path through the step launcher, as a hook step runs. A daemon lists the installers
it can run in a new heartbeat field, `HeartbeatRequest.installers = 8`: an installer whose tool is present at its
absolute path when the daemon starts (`/usr/bin/dpkg`, `/usr/bin/rpm`, `/usr/sbin/installer`, `/usr/bin/podman`). The
coordinator orders a target only to a host that lists its installer. A list, not a feature per installer, because the
set depends on the host's tools and grows with every format, and the feature list is capped at 16 names, of which ten
are taken once `target-v1` is added.

**Why built in, and not delivered.** The daemon runs only code it shipped with or code inside bytes a trusted signer
attested. An installer is root code that every service's release goes through, so it is shipped with the daemon and
covered by the daemon's own attestation. An installer delivered as a package of its own, pinned by digest in the
manifest, would be just as attested, but the manifest is not (section 8.2), so it would let the deploy caller choose
which attested program installs everything. A built-in installer narrows that choice to a closed list.

**A new format without a hostkeeper release.** A service ships a `tar` artifact that holds its bytes in the new format
and a hook that installs them with the format's tool. The bytes are attested; the command that runs them (the hook
program, its arguments, its bounds) is chosen by the target's unattested manifest, as for any `tar` target (section
8.2). Its `uninstall` step removes what it installed: closed-world removal of the format's files is then the hook's
duty, checked by the service's conformance tests, and hostkeeper removes its own tree and links. When several services
ship one format this way, it becomes a built-in installer in a hostkeeper release.

**One installer per service.** The daemon records a service's installer at its first install (section 6.5) and refuses
an order with another installer for that service, `Failed`, `release`, `installer_changed`, until the service is
uninstalled. So a service installed from attested archives cannot be relabelled `tar` by a later call.

### 5.2 The built-in set

| Installer | Artifact | `stage` | `activate` | `remove` | Lists it |
|---|---|---|---|---|---|
| `hostkeeper-archive` | Today's `tar.zst` with `manifest.toml` inside | Today's unpack, bounds and shape rules, exactly (`hostkeeper-unpack`). The hook, units, files and params come from the attested `manifest.toml` inside, and the target's manifest carries only `installer` | Today's switch and placement | Today's uninstall | Every daemon from A4 |
| `tar` | A tar, plain or with gzip, xz or zstd: a plain tarball, a Homebrew bottle | `hostkeeper-unpack`'s bounded reader, extended in A6 with gzip and xz decoders, plain tar, a mode with no fixed top-level layout, and symbolic links (section 6.1) | Switch `current`, link the `place` entries, reload | Unlink what is placed | Every daemon from A6 |
| `deb` | A `.deb` | `dpkg-deb --extract` into the tree (no maintainer script runs); `dpkg-deb --field <artifact> Package` must equal `package` | `dpkg --install <artifact>`; record the installed version from `dpkg-query` | `dpkg --purge <package>`, only while `dpkg-query` shows the version hostkeeper recorded | Linux with `/usr/bin/dpkg` |
| `rpm` | An `.rpm` | `rpm2archive` into the bounded tar reader; `rpm --query --package --queryformat %{NAME}` must equal `package` | `rpm --upgrade --oldpackage <artifact>`; record the installed version | `rpm --erase <package>`, under the same condition | Linux with `/usr/bin/rpm` |
| `macos-pkg` | A flat `.pkg` | `pkgutil --expand-full` into the tree; its package identifier must equal `package` | Refuse if a path the payload installs exists on the host and is not recorded for this service (`package_conflict`); `installer -pkg <artifact> -target /`; record every file `pkgutil --files <package>` lists, with its digest | Delete each recorded file whose digest is unchanged, then `pkgutil --forget <package>` | macOS |
| `oci-image` | An OCI image archive (`oci-archive` tar) holding exactly one image | Keep the file; read the archive's `index.json` with the bounded reader and refuse any count of images but one (`manifest_invalid`) | Note whether the runtime already holds the image's digest; `podman load --input <artifact>`; write one unit per `containers` entry; reload | Remove the generated units; `podman rmi` an image only if this service's `activate` loaded it and no kept version uses it | Linux with `/usr/bin/podman` |

The `retire` step is the same for every installer: delete the version directory, its seal and its kept artifact. For
`oci-image` it also removes the version's image, under `remove`'s rule. No other installer's `retire` calls the
format's tool, since the format's database holds only the installed version.

**The extracted tree.** Every installer's `stage` ends the same way, by the core and not by the installer: the daemon
walks the staging directory with the bounded walker of `hostkeeper-unpack`. It refuses an entry under a link, a link
whose target is absolute or leaves the tree, a hard link, a device or a fifo, and more than `max_entries` entries or
`max_unpacked_bytes` bytes. It makes every entry root-owned, strips setuid, setgid and sticky bits, and sets modes to
0555 or 0444, as unpack does today. On macOS it runs today's `MacVerify` on the whole tree (no quarantine attribute, a
valid code signature on every Mach-O file). On Linux the extractor runs in a transient unit with `ProtectSystem=strict`
and `ReadWritePaths=` the staging directory only, so it cannot write elsewhere. On macOS `pkgutil --expand-full`
writes only into the directory it is given, and the walk refuses anything else. An extraction over the bounds may fill
the staging directory before the walk sees it: the bounds are checked after, the directory is then removed, and
`stage_secs` bounds the time. The tree is a copy for the hook and the observer; the host's installed files are what the
format's tool installs in `activate`.

**The generated unit of `oci-image`** is hostkeeper's fixed text, `ExecStart=/usr/bin/podman run --rm --pull=never
--name <unit> [--publish <address>:<host>:<container>]… [--volume <host_path>:<container_path>[:ro]]… <image>@sha256:<hex>
<args>…`. The `args` follow the image, so the runtime passes them to the image's entry point and never reads them as its
own options. The address is `127.0.0.1` when `loopback_only` is set, otherwise every address. No option that grants the
host's privileges (`--privileged`, a host network, a device, a capability) can be expressed.

A dependency a `.deb` or `.rpm` names must already be on the host: in the boot image, or a hostkeeper service of its
own. The daemon never runs `apt` or `dnf`, so no installer fetches anything. A missing dependency fails `activate` with
`Failed`, `environment`, `dependency_missing`, and the version is rolled back (section 6.4).

`activate`, `remove` and `retire` of `deb`, `rpm` and `macos-pkg` hold a host-wide installer lock, so two services never
run `dpkg` at once. The lock is held only for the tool's run.

### 5.3 What no installer may do

- Read a byte of the artifact before the core has checked its size, its digest and its attestation (section 7.4).
- Run a script of the package in `stage`. The format's scripts run only in `activate` and `remove`, under their bound.
- Fetch anything, or let the format's tool fetch (no `apt`, `dnf`, `brew` or registry pull).
- Write outside the version's staging directory in `stage` (enforced by the unit's sandbox on Linux, and checked by the
  walk on both platforms).
- Remove in `remove` anything its `activate` did not record.

## 6. An update, a rollback and an uninstall

### 6.1 The version directory

For every installer but `hostkeeper-archive`:

```
/var/lib/hostkeeper/packages/<service>/<version>/     0555, sealed
  target.json        the order's target, manifest included, as canonical JSON
  artifact           the verified bytes, 0444
  tree/              what stage extracted: the hook, and the files the observer hashes
```

A `hostkeeper-archive` version keeps today's layout exactly: the unpacked archive at the version root (`manifest.toml`,
`bin/`, `units/`, `files/`), no `target.json`, no `artifact`, no `tree/`. Its seal's `package_digest` is the target's
digest, and its hook program is relative to the version root, as today. So an older binary reads it unchanged, and a
version directory with no `target.json` is read as `hostkeeper-archive`, which is also how a new daemon reads every
version staged before A4. One loader, `VersionDir::read` in A6, replaces today's eight `Manifest::load` calls on a
version directory (order, boot resume, boot step, force, force switch, uninstall, observer, placer); it reads
`target.json` when present and `manifest.toml` otherwise.

The seal and `Staged::check` cover the whole directory, so the hook program, the kept artifact and the manifest are
re-hashed before every step, as the tree is today. The tree digest gains one record kind, `l`, for a symbolic link,
hashed by its target text, and `Staged::check`'s walk accepts a link only inside `tree/`, never follows one, and still
refuses a foreign owner, a setuid or setgid bit and a group- or other-writable entry (the walk of section 5.2 made the
tree so). Extracted `.deb`, `.rpm` and `.pkg` trees hold links, and today's walk refuses them. A tree with a link was
never readable by an older daemon, which is never ordered one (section 9.1), so the change is additive. A hook program
and a `place` source still must not pass through a link.

**A version already on the host.** An order for a version whose directory is sealed uses the sealed `target.json`, not
the order's manifest. The daemon compares the order's target with it field for field and refuses a difference,
`Failed`, `release`, `target_changed`. A rollback to a version on the host reads only the sealed copy.

The step record is unchanged. An installer step's progress is kept in a file of its own,
`/var/lib/updater/state/<service>/install-record.json`, which an older daemon does not read. Every installer step is
idempotent, so a step found running at start is run again.

### 6.2 The boundary: the installer puts bytes on the host, the hook hands work over

An update runs, in order:

1. Fetch the artifact and its bundle, verify both (section 7.4).
2. Check the manifest (section 4.4). Installer `stage`, then seal.
3. Hook `prepare`, from the new version's tree.
4. Installer `activate` of the new version. This takes the place of today's switch of `current` before `cutover`.
5. Hook `cutover`, `verify`, `cleanup`, as today.
6. After a confirmed switch: installer `retire` of every version but `current` and `previous` (today's prune).

The installer changes what is on disk. The hook changes what runs: it stops the old processes, drains, migrates, starts
the new ones and says whether the new release serves. A package whose own scripts restart the service (a `postinst`
running `deb-systemd-invoke restart`) does the hand-over outside the hook. hostkeeper does not suppress that. A service
that needs the hook's hand-over builds its packages so their scripts do not start or restart its units (debhelper's
`--no-start --no-restart-after-upgrade`, an `.rpm` without `%systemd_postun_with_restart`), and its `cutover` restarts
them. The service's conformance tests check it. A native package's scripts also enable its units at boot; the daemon
enables only the units it places (`tar`, `hostkeeper-archive`), and the placer's unit lint applies only to those.

### 6.3 Built-in steps, for a service with no hand-over of its own

A manifest whose `hook.command` is empty gets the daemon's built-in steps, which are shipped code like an installer:
`prepare` and `cleanup` do nothing; `cutover` restarts every `run` unit and waits for it to be active within its bound;
`verify` passes when every `run` unit is active and `health_url`, if any, answers 2xx, and defers otherwise until
`verify_deadline_secs`; `rollback` restarts the units again, after the switch back; `uninstall` does nothing. This is
what most packaged services need, and what an OCI image needs, since an image carries no hook.

A service on built-in steps can always be uninstalled, whatever `timeout_secs.uninstall` says, because its `uninstall`
runs nothing. A service with a hook program and no `uninstall` bound is refused `uninstall_unsupported`, as today.

### 6.4 What "previous version kept for rollback" means

The previous version's directory keeps its verified artifact, so a rollback never fetches, for every format:

| Installer | Rollback, before the hook's `rollback` step |
|---|---|
| `hostkeeper-archive`, `tar` | Switch `current` back and relink, as today |
| `deb`, `rpm` | Install the kept previous artifact: a downgrade by one version (`dpkg --install`, `rpm --upgrade --oldpackage`), running the older package's scripts. A service that ships these formats keeps its scripts able to go back one version, as its hook must read the record one version back today |
| `macos-pkg` | Install the kept previous `.pkg`, then delete each file the newer receipt recorded and the older does not, if unchanged since |
| `oci-image` | The previous image is still loaded (`retire` keeps it while `previous` names it). Re-point the generated units at it and reload |

A first install that fails has no previous version: `remove` takes it off the host, as `remove_all` does today.

### 6.5 Uninstall and closed-world removal

Uninstall keeps today's order (`crates/hostkeeper-daemon/src/uninstall.rs`): tombstone, stop the `run` units, the hook's
`uninstall`, then installer `remove` of the service, then delete the version directories, the step record,
`install-record.json` and the service's `installed` entry. Each is durable before the next and resumes at the
tombstone; `remove` is idempotent, so a resume after `hook_done` runs it again. A service of any installer but
`hostkeeper-archive` is tombstoned under a new key, `uninstalling_installed`, which an older binary keeps when it
rewrites `placed.json` and does not act on. So an older binary after a fallback never finishes such an uninstall by
deleting the version directories while the package stays installed.

Each installer removes what the format's tool records for the package, plus what hostkeeper recorded at `activate`,
and nothing else. hostkeeper records in `placed.json`, under a new additive key `installed`, each service's installer
and, for a native format, the package identity and the version it installed (and, for `macos-pkg`, the file list with
digests; for `oci-image`, whether it loaded the image).

- `tar` and `hostkeeper-archive`: the links `placed.json` lists, while each is still the recorded link, as today.
- `deb` and `rpm`: the package the manifest names, purged with `dpkg --purge` or erased with `rpm --erase`. The
  format's database is the list of the package's files. hostkeeper refuses to install a package that is installed on
  the host but not recorded as its own (`Failed`, `release`, `package_not_ours`), or that another service records
  (`package_conflict`), and it removes only while the installed version is the one it recorded. A package someone
  upgraded by hand is left alone and raises `installed_changed`.
- `macos-pkg`: macOS has no removal of a package, so `activate` refuses to install over a file it does not own and
  records every file the receipt lists, with its digest. `remove` deletes a recorded file only while its digest is the
  recorded one, then forgets the receipt. A file changed since is left alone and reported. A directory is removed only
  when it is empty and was recorded as created by the install.
- `oci-image`: the generated units, and the image only if its own `activate` loaded it.

What stays, by each format's own rules: files a package's scripts made that the format does not list (a user, a cache,
an alternative), an `.rpm`'s configuration files changed since install, which `rpm` keeps as `.rpmsave`, and the
service's data under `/var/lib/<service>`, which is the hook's business, never the daemon's, as today. A package's own
removal scripts may delete what they made; that is the service's packaging choice. The device tests of section 12 show
the fixture packages leave nothing behind.

### 6.6 Observation

The observer finds the first `run` unit's main program, as today, and hashes the running process's executable against
the same path in the version's sealed tree. For `tar` and `hostkeeper-archive` the unit file is the placed one, as
today. For a native format it reads the unit text from the extracted tree, at the path the package installs it
(`tree/lib/systemd/system/<unit>`, `tree/usr/lib/systemd/system/<unit>`, or `tree/Library/LaunchDaemons/<label>.plist`),
and maps its program onto the tree: for a `.deb` that installs `/usr/bin/tierstore`, `tree/usr/bin/tierstore`. The
extracted tree is the attested package's files, so a process left from another version still shows `version_mismatch`.
For an `oci-image` unit, the running version is the image digest the runtime reports.

## 7. Stores: where the bytes are and how the daemon fetches them

Kept from #179 section 7, but the attestation bundle is a `Blob` of the target, fetched like the artifact.

### 7.1 Locations

A location names a store the host's own configuration defines, and a locator relative to it. A locator is 1 to 1024
bytes of URL path characters. It has no scheme, no `//`, no query, no fragment, no `%` and no `.` or `..` segment, and
a daemon given any other answers the order `Failed`, `release`, `order_invalid`, and runs nothing. A target the daemon
cannot accept is reported that way, not dropped as an order of an unknown verb is today, so the coordinator and CI see
it. The header of `agent.proto` says an order is "never a command, a path, a URL or a script". A4 rewrites it to: "An
order names bytes by digest and where to look for them. It never carries an absolute URL, a credential or a script. A
target's manifest names a program inside the attested bytes to run as root, its arguments, and paths of the host,
under the checks of the daemon; whoever sets the release chooses them." Section 8.2 says what that means.

### 7.2 The host's stores: `/etc/hostkeeper/stores.toml`

```toml
# root:root 0644; credentials in their own 0600 files
[[store]]
name = "github"                      # the name a location uses
kind = "https"                       # https or oci
base = "https://github.com/"
redirects = 3                        # https only, at most 5; 0 is today's rule and the default

[[store]]
name   = "lan"
kind   = "oci"
base   = "https://mirror.example/"   # a tierstore OCI front, or any registry
mirror = true                        # tried first for every digest, listed in an order or not
repository = "releases"              # oci mirror only: the name in /v2/<repository>/blobs/...
basic_auth_file = "/etc/hostkeeper/stores/lan.basic"   # user:secret, root 0600
# or bearer_file = "/etc/hostkeeper/stores/lan.token"; at most one of the two; neither means anonymous
```

The stores live in a new file, not in `hostkeeper.toml`, which refuses unknown keys and which the previous binary reads
after a self-update falls back. A daemon with no `stores.toml` has one store, named `channel`, of kind `https`, at its
`release_channel`, with no redirects: today's fetch. The file is read at start. A store of unknown kind, a base that is
not `https://`, userinfo in the base, a credential file that is not root-owned and 0600, both credential keys on one
store, an `oci` mirror with no `repository`, or two stores of one name refuses the start (rule S2: configuration in
files, not environment).

### 7.3 Store kinds and authentication

| Kind | GET for a blob | Fits |
|---|---|---|
| `https` | `<base><locator>`, following up to `redirects` redirects, each to `https://` | GitHub release assets (which redirect to the asset host), today's channel tree, any static origin |
| `oci` | `<base>v2/<locator>/blobs/sha256:<hex>`; the locator is a repository name; a mirror uses its `repository` | tierstore's OCI front, any OCI registry |

- **To a store:** with that store's credential file, or anonymously. The credential is the host's own, read-only. The
  coordinator never holds or sends one. A credential is sent only to its store's base origin and dropped on a redirect
  to another origin.
- **An OCI registry's token challenge** is answered only when the realm's origin is the store's base origin.
- **To the coordinator:** mutual TLS with the host's enrolled certificate, unchanged.
- **Of the store, to the daemon:** TLS with the host's CA bundle. That authenticates nothing that matters: the bytes are
  trusted only once the digest and the attestation check out.

S3 with SigV4 is not in v1: a presigned URL would put a credential in an order. A LAN mirror fills by pulling
(`GetRelease`), and its sync job is the mirror operator's, outside this design's build items.

### 7.4 The fetch and the proof, in order

For the artifact and its bundle, the daemon tries its `mirror` stores in file order, then the target's locations in
order, skipping a store it does not define, and takes the first source whose bytes have the expected size and SHA-256.

| What happened | Outcome |
|---|---|
| Some source gave the right bytes | Go on. A source that gave other bytes first raises `store_digest_mismatch`, naming the store |
| No source is defined on the host | `Failed`, `environment`, `no_store` |
| Every source failed to answer | `Deferred`, `environment`, `fetch_failed`, as today |
| Every source that answered gave other bytes, and one was a location of the target | `Failed`, `release`, `package_digest_mismatch`, as today |
| Only mirrors answered, all with other bytes | `Deferred`, `environment`, `store_digest_mismatch` |
| More bytes than `bytes`, or than `max_package_bytes` (a bundle: its `bytes`, at most 1 MiB) | Stop; that source counts as giving other bytes |

Each source gets its own `fetch_timeout_secs` (`attestation_timeout_secs` for a bundle). Then the daemon verifies the
bundle with the pinned `gh attestation verify` against `[trusted_signers]`, as `prove.rs` does today; the bundle must
list the artifact's digest. Only then does `VerifiedArchive` exist, renamed `VerifiedArtifact`, and only it reaches an
installer. The digest and the attestation are checked for every installer the same way, by the same code.

## 8. The guarantees

### 8.1 What keeps each

| Guarantee | Today | After |
|---|---|---|
| An order names exact bytes | `package_digest` | `package_digest`, and `target.digest` equal to it (an order where they differ is `Failed`, `release`, `order_invalid`) |
| Digest and attestation before any byte is read | `fetch.rs`, `prove.rs`, `VerifiedArchive`, then `unpack` | The same, for every installer (section 7.4) |
| Only the trusted signers' builds run | `[trusted_signers]`, the pinned `gh`, the fixed issuer and predicate | Unchanged. The attestation field is a lookup; what it is checked against is the host's root-owned config |
| The host runs its platform's build | The daemon takes its own platform's index entry | The coordinator sends the target of the platform the daemon reports from its compile target; an installer and its format's tool refuse bytes for another architecture (`dpkg`, `rpm`, `installer`, `podman` do), and the observer reports a program that does not run |
| What runs as root | The hook the attested `manifest.toml` names, from attested bytes | The format's scripts, from attested bytes, and a program of the attested tree with arguments the unattested manifest chooses (section 8.2); for `hostkeeper-archive`, unchanged |
| Removal | The links `placed.json` records | What each installer recorded at `activate` (section 6.5) |

**What the attestation proves that the digest does not.** The digest says which bytes. Anyone who can make bytes can
give their digest: a caller of the deploy API, or someone who controls the coordinator. The attestation says this
organisation's CI built those bytes, from a commit of its repositories, on a ref and in a workflow `[trusted_signers]`
allows. Without it, whoever can call `SetDesired` could point hosts at bytes they built themselves. With it, they can
only point hosts at bytes that went through CI.

### 8.2 The manifest is not attested

The manifest comes from the deploy call, not from the attested bytes. Whoever can call `SetDesired` for a service (CI
with an OIDC token its allow-list entry admits for that service, an operator token, or anyone who controls the
coordinator) decides the manifest. That is a change: today the signer's bytes decide the hook and the units.

What that caller can decide:

- which program of the attested tree runs as root as the hook, with which leading arguments, and with which bounds;
- which files of a tarball's tree are linked to which paths outside the refused prefixes, and which of its units are
  placed;
- which `run` units start, a container's arguments, ports and volumes, the params and the health URL.

What that caller cannot do: run a byte no trusted signer built, run a program outside the attested tree, choose the
installer's code, or reach a path the checks of section 4.4 refuse.

The plain consequence: most artifacts hold a program that runs what it is told (a shell, an interpreter, the service's
own binary with a script flag). So **calling `SetDesired` with targets for a service is as good as running commands as
root on that service's hosts.** The same is true of anyone who controls the coordinator. The path guards and the name
rules are guards against mistakes, as they are today, and not a boundary against such a caller. What bounds the caller
is who may call: the OIDC allow-list names the service, the repository, the workflow and the ref, and every set with
targets becomes an audit entry recording each manifest's digest (section 3.4).

The `hostkeeper-archive` installer keeps its manifest inside the attested archive. A service already installed from
archives keeps today's guarantee, because a host refuses a later order of another installer for it (section 5.1), and
the service `hostkeeper` may only be set with archives (section 3.4). A service's first install takes whatever
installer the caller names. Question 1 (section 15) asks whether CI should also attest each manifest, which would bring the
same guarantee to every format at the cost of one more bundle per target.

## 9. Compatibility and the staged path

### 9.1 What an old daemon reads, and what it must keep getting

A daemon before stage 3 (the daemons on hosts now):

- announces no `target-v1` and lists no installers;
- fetches `{release_channel}/S/V/index.json`, which must stay strict-readable: its four keys, platforms among the three,
  at most one entry each, `file` equal to `S-V-{platform}.tar.zst`;
- reads `manifest.toml` in every version directory of every service at start, in the observer, in force and in
  uninstall, and refuses one with an unknown key;
- skips the `target` field of an order over gRPC (prost skips unknown fields); the JSON reply, which is read strictly,
  never carries one (section 3.3).

So an old daemon is only ever ordered a `hostkeeper-archive` target. It is ordered as today: the target's digest as
`package_digest`, and the release's index digest as `index_digest` (empty when the set carried no index, as for a set
with `package_digest` today). The channel goes on holding the index and the archives while such daemons remain
(stage 5). A target of another installer shows such a host `target_unsupported`.

**An earlier binary on a host's disk.** A host may hold versions of an installer an earlier binary does not have: any
installer but `hostkeeper-archive` for a binary before A4, and `deb` for a binary before A7, and so on. Such a binary
runs there in two cases.

- **A rollback of the service `hostkeeper`.** The daemon refuses it, before it switches anything, when the target
  release is older than the first release of hostkeeper that has an installer of a version the host holds:
  `Failed`, `release`, `rollback_below_installer`. The daemon knows its disk, and it carries a compiled table of the
  release each installer arrived in. An operator who must go back uninstalls those services first.
- **A fallback after a self-update fails.** It runs the binary before the one that failed, and cannot be refused. When
  that binary lacks an installer of a version the host holds (the newer binary installed it before failing), the
  earlier binary reports those services with an unreadable release and leaves them as they are, and the daemon and
  every other service run on. It does not uninstall them (section 6.5).

A4 has the test that starts an earlier release's binary on a host holding a non-archive version, and A7 to A10 extend
it to their installers.

**An old coordinator.** A coordinator rolled back to before A2 decodes at most 4 KiB on every operator method, so it
refuses a set with targets for its size on both routes (`RESOURCE_EXHAUSTED` over gRPC, `invalid_request` on the
gateway, which also refuses the unknown field). CI's call fails loudly and nothing is set until the coordinator is
rolled forward or CI falls back to today's `release_index` form, which every coordinator takes.

### 9.2 The stages

Each stage is build items of section 13, released in order, coordinator first; a stage starts once the previous one is
on every host (`Hosts.List`).

| Stage | Released | Old behaviour still answered | Safe because |
|---|---|---|---|
| 1 | Coordinator: `SetDesired` with targets, releases kept, `GetRelease`, `target-v1` echoed, `installers` read, targets in orders over gRPC, `no_target`, `target_unsupported` and `installer_unsupported` (A1, A2, A3) | Every set with a digest or an index; old daemons ordered exactly as today | Every change is additive; no daemon announces `target-v1` yet, so no order carries a target |
| 2 | CI of this repository and of `hello` sets every release with `hostkeeper-archive` targets and the release index (A5) | `channel-sync` keeps filling the channel | The targets are the same bytes the index lists, and the coordinator checks that they agree (section 3.4) |
| 3 | Daemon: `target-v1`, `stores.toml`, fetch and proof through the target, `hostkeeper-archive` through the target (A4) | An order without a target is fetched through the channel as today | The old daemon updates to it through the channel. The new one is sent targets only once it announces `target-v1` and the coordinator echoes it |
| 4 | Installers: `tar` and built-in steps (A6), then `deb`, `rpm`, `macos-pkg`, `oci-image` (A7 to A10) | Archives keep being set for old daemons | A target is ordered only to a daemon that lists its installer |
| 5 | Retire the channel for every service but `hostkeeper` (A11) | A set with an index or a digest is still accepted (rule B8). The channel keeps hostkeeper's own archives, so a host installed from an older deb, rpm or pkg can still update itself | Every host announces `target-v1`, and each retired service's last three releases answer `GetRelease`, both shown in A11's pull request |
| 6 | Daemon: drop the channel fetch and the index parser's `Platform` copy (A12) | The channel of hostkeeper's archives stays for old bootstrap packages | The daemon requires `target-v1` echoed; against an older coordinator its orders are `Deferred`, `coordinator_too_old` |

### 9.3 Self-update at every stage

hostkeeper updates itself as the service `hostkeeper`. Its own release stays `hostkeeper-archive` on all three
platforms at every stage: the self-update path needs `bin/hostkeeper` in a sealed tree, and an old daemon must always
be able to fetch the next release. The deb, rpm and pkg that bootstrap a host are not installed through an installer;
they are what installs the daemon in the first place.

Stages 1, 2 and 4 to 6 change nothing for hostkeeper's own update. At stage 3 an old daemon fetches the new release's
archive through the channel, probes it with `self-update-protocol`, switches and restarts into it. If the new binary
fails to start, the fallback runs the old one, which reads `hostkeeper.toml` (unchanged, since the stores are in
`stores.toml`), the step record (unchanged) and `placed.json` (whose unknown keys it keeps).

The device tests on every pull request (`pr-device.yml`, jobs `privileged` and `macos-device`) prove each stage:
`a_daemon_the_deb_installed_is_installed_its_next_release_and_restarts_into_it` (`self_update.rs`),
`a_daemon_the_pkg_installed_is_installed_its_next_release_and_restarts_into_it` (`device_mac_selfupdate.rs`), and
`a_next_binary_that_cannot_start_spends_the_start_limit_and_the_fallback_puts_the_old_one_back` (`self_update.rs`), run
from stage 3 on with the next release set by targets. A4 adds the mixed cases: the previous binary updated by a
stage-3 coordinator and back by the fallback; the previous binary started on, checking, rolling back to and
uninstalling an archive version the new daemon installed, including a placed `current/units/...` link and hostkeeper's
own `bin/hostkeeper` staging; and an earlier binary started on a host holding a non-archive version (section 9.1).

### 9.4 Proto compatibility and the baseline

Every proto change is additive (rule B8, and the "Extending" rules of `agent.proto` and `operator.proto`):

- new file `release.proto`: `Target`, `Blob`, `Location`, `Manifest`, `Hook`, `StepBounds`, `InstallerBounds`,
  `Place`, `Param`, `Container`, `Port`, `Volume`;
- `operator.proto`: `SetDesiredRequest.targets = 8`; the RPC `Releases.GetRelease` and its messages;
  `Host.installers = 18`; `ReleasesRefusal` values `TARGET_INVALID = 23`, `RELEASE_EXISTS = 24` and
  `RELEASE_UNKNOWN = 25`; `GateReason` values `GATE_REASON_NO_TARGET = 31`, `GATE_REASON_INSTALLER_UNSUPPORTED = 32`
  and `GATE_REASON_TARGET_UNSUPPORTED = 33`. `GATE_REASON_NO_PACKAGE_FOR_PLATFORM = 24` keeps its meaning, for sets
  made with an index, and is not widened;
- `agent.proto`: `HeartbeatRequest.installers = 8`; `target = 5` on `Update`, `Install` and `ForceRollback`, and
  `target = 6` on `Force`; the feature `target-v1`; the header's sentence on what an order carries (section 7.1).

The host's files change additively too: `placed.json` gains the keys `installed` and `uninstalling_installed`, which an
older binary keeps on rewrite (`crates/hostkeeper-placed/src/store.rs`); `install-record.json` and `target.json` are new
files an older binary does not read; the tree digest gains the `l` record, in trees an older binary never holds.

No field is renumbered, renamed, retyped or removed. The baseline `crates/hostkeeper-proto/proto/compat/baseline.binpb`
does not change in any pull request; `the_protos_keep_every_rule_against_the_baseline` passes on each item unchanged.
Each item that adds proto adds an `additions_*.rs` test (`additions_targets.rs`, `additions_installers.rs`): a reader
built from the baseline, standing for the previous release, keeps the new fields and writes them back unchanged, and the
new reader takes the old messages. Those are the B8 tests in both directions for the wire. For the coordinator's state,
A2 has a test that starts the previous coordinator's reader on a state that has releases, on the file store and on
Postgres (section 3.6). For the host, A4 and A6 test the previous binary against `placed.json` with `installed` and
with an `uninstalling_installed` tombstone (it leaves the package and the records untouched), a version with
`target.json`, and a tree with an `l` record. The frozen JSON routes `/v1/heartbeat` and `/v1/desired` gain nothing.

## 10. Fit with the two-programs design and the hook contract

The two-programs design (`docs/design/two-programs.md`) generates every client from the proto. This design adds no
verb: the deploy call is a proto method, installers run inside the daemon. Only A5 waits for the generated bash
client; until it exists, CI keeps today's `set-desired` path.

The hook contract (`crates/hostkeeper-hook`, hostkeeper-design.md section 5.3, host-updates.md section 3.2) keeps its
steps, command line, exit codes, last line and params file, and gains no reserved `updater.*` key. A hook's `<artifact>`
argument and `updater.staged_release` are the version directory, as today; its program is in `tree/`, or at the version
root for `hostkeeper-archive`. Two things are added beside it:

- **Installer steps** (`stage`, `activate`, `retire`, `remove`) follow the same contract (idempotent, bounded, run in transient
  units, an outcome with class and reason), and run where today's unpack, switch and prune run. A hook never runs one.
- **Built-in steps** (section 6.3) answer the five steps for a manifest with an empty `command`. They are the daemon's
  code, as two-programs.md stage 1 has the daemon answer hostkeeper's own steps.

AGENTS.md rule B17 asks for one generic updater with per-service hooks declared in a root-owned file. The declaration
moves from the sealed `manifest.toml` to the sealed `target.json` in the version directory, still root-owned and
re-hashed before every step; the updater stays generic, and the per-format code is the closed set of installers.

## 11. Later: host facts and variant builds

#179 sections 3 and 4 designed host facts (ISA level, OS version, libc, accelerators) and builds chosen by "most specific
wins". v1 does not need them: a release has one target per platform. If a service later needs variants within a
platform (an x86-64-v3 build, a CUDA build), the additive route is a new field beside `targets`, for example
`map<string, TargetList> variants = 9`, whose entries carry constraints over facts the heartbeat reports and are chosen
as #179 section 4.2 says, with the platform's entry in `targets` as the fallback. `targets` itself never changes type
(rule B8). Until then, choosing among variants at run time stays
the build's business (hostkeeper-design.md section 5.5).

## 12. Tests

Each item's tests are behaviour tests in Rust, in the crate the item changes (rules T8, T11, T12). Each runs with no
network and no system path, through the fakes the crates already have (`Fetcher`, `Verifier`, the coordinator's backend
fake, the step launcher's fake, the placer's roots under a scratch directory), and through a command port for the
formats' tools. None asserts a time: a bound is tested on an injected clock (rule T10). Real `dpkg`, `rpm`, `installer`
and `podman` run only in the device jobs (`privileged`, `macos-device`), whose tests count toward no coverage gate.

- **Manifest and target checks (A1; `crates/hostkeeper-release/tests/manifest.rs`).** One case per check of sections
  3.4 and 4.4: each field an installer requires missing, each it refuses present; an unknown installer; `min_hostkeeper`
  above the daemon (`daemon_too_old`, not `manifest_invalid`, even with another field wrong); every bound at 0 and at
  its limit, and `verify_deadline_secs` one below `verify + rollback`; a hook path with `..`, `.`, a leading `/`,
  through a link, a directory, missing from `tree/`; a hook `command` for `oci-image`; a unit name of another service,
  without a suffix, with a bad instance; a container `unit` not in `run`; a file path under each refused prefix and one
  beside it; a volume host path outside `/var/lib/<service>/` or with `..`; a host port 1023 and 65536; a health URL
  to a non-loopback host; an `updater.` param; each count limit one under and one over; a manifest of 16 KiB and one
  byte over; a target of 24 KiB and one byte over; 2 KiB of locators and one byte over; an attestation absent, of
  0 bytes, of 1 MiB and one byte over; 9 locations.
- **Deploy call (A2; `crates/hostkeeper-coordinator/tests/targets.rs`).** Every row of section 3.4, one refusal each, a
  fourth platform key, no target at all, and `hostkeeper` with a `tar` target. An identical repeat is accepted; a
  different artifact or manifest under the same version is `release_exists`. An index that disagrees by one digest, one
  size or one platform is refused, and one that agrees is accepted, including an index of two platforms with the third
  null. The JSON route with `"linux-arm64": null` and with the key left out give the same release. A body of 96 KiB is
  accepted and one byte over refused, on both routes, and every other method is still refused over 4 KiB. A set with
  targets by CI and by an operator each writes one audit entry with each manifest's digest. `GetRelease` of an unknown
  service is `unknown_service`, and of a version never set with targets `release_unknown`. The previous coordinator
  starts on the new state, on the file store and on Postgres, and shows the version `no_digest`.
- **Ordering (A3; `crates/hostkeeper-coordinator/tests/target_orders.rs`).** Over a mixed fleet (old and new daemons,
  three platforms, hosts with and without `dpkg`): a host of a platform with no target is `no_target`, not failed, not
  missing, and does not hold convergence; a host without `target-v1` given a `deb` target is `target_unsupported`, and
  one with `target-v1` that lists no `deb` is `installer_unsupported`, both named by `Converge`; an old daemon is
  ordered a `hostkeeper-archive` target as today, with the index digest, and with none when the set had no index; a
  target is sent only to a daemon that announced `target-v1`, only over gRPC, and the feature is echoed; one host
  beating over gRPC, then JSON, then gRPC keeps its first installers list, gets an archive order in today's form and no
  `deb` order over JSON, and the `deb` order in the next gRPC reply; a `Force` to a version set with targets follows
  the same rules; a reply with the largest valid target fits in 32 KiB.
- **Daemon fetch and proof (A4; `crates/hostkeeper-daemon/tests/stores.rs`, `crates/hostkeeper-package/tests/fetch.rs`).**
  Every row of section 7.4's table: a mirror wins; a mirror with wrong bytes then a right location; only mirrors wrong;
  a hung mirror then a location; every source down; every location wrong; no store; one byte over `bytes`; a redirect to
  `http://` and one past the count; a credential not sent after a cross-origin redirect; a realm on another origin; a
  bundle over 1 MiB; each refused `stores.toml`; no `stores.toml`. On the order: a target whose digest is not
  `package_digest`, a locator with `..`, `%`, a scheme, `//`, a query or 1025 bytes (`order_invalid`, reported); a
  manifest refused before any fetch; a bundle that does not list the digest (`not_attested`); an order without a target
  takes the channel; a reply of 32 KiB read and one byte over refused. On earlier binaries: the mixed cases of
  section 9.3.
- **Installers (A6 to A10; `crates/hostkeeper-install/tests/<installer>.rs`, device tests in
  `crates/hostkeeper-privileged/tests/install_<installer>.rs` and the macOS device suite).** For each installer: the
  installers list holds it when its tool is at its path at start, and not when the tool is missing. `stage` refuses a
  package whose identity is not `package` (and an OCI archive of two images), and runs no script (the fake tool records
  its calls). The walk after `stage` refuses an entry under a link, an absolute or escaping link, a hard link, a device,
  one entry over `max_entries` and one byte over `max_unpacked_bytes`; it makes a non-root owner root, strips a setuid
  bit and sets modes; on macOS an unsigned Mach-O file is refused. `activate` twice is the same as once, and a crash
  between the tool and the switch resumes to the same state. An `activate` that fails rolls back to `previous`, and a
  first install that fails is removed. A tool past its bound is stopped and the step fails `environment`. Two
  services' `activate` never run the tool at once. Rollback installs the kept previous artifact with no fetch; for
  `macos-pkg`, a file only the newer receipt lists is deleted and a changed one left. `retire` of version N-2 after
  N-1 and N leaves every installed file of N untouched and never calls the tool. `remove` removes what `activate`
  recorded, and leaves and reports a file changed since (`macos-pkg`), a package upgraded by hand (`deb`, `rpm`), or an
  image another kept version uses or the runtime held before (`oci-image`). Then `package_not_ours`,
  `package_conflict`, `installer_changed`, `dependency_missing` (`deb`, `rpm`), and `target_changed` for a sealed
  version ordered again with another manifest. The generated container unit for each combination of `args`, ports
  (loopback or not) and volumes (read-only or not) equals its golden text. The observer hashes the program a native
  unit names in `tree/` and reports `version_mismatch` for a process of another version, and the image digest for
  `oci-image`. The device tests install, update, roll back and uninstall a two-version `hello` in each format on the
  hosted runner, and assert that nothing the fixture package's installer put there remains after uninstall.
- **Tree and seal (A6; `crates/hostkeeper-unpack/tests/formats.rs` for the digest goldens,
  `crates/hostkeeper-steps/tests/artifact.rs` for the walk).** A tree with a link has a fixed digest, and a changed link
  target changes it; `the_tree_digest_has_a_fixed_value` holds unchanged for a tree without links; `Staged::check`
  accepts a link inside `tree/`, refuses one elsewhere, and refuses a hook path or `place` source through a link.
- **Built-in steps and uninstall (A6; `crates/hostkeeper-daemon/tests/builtin_steps.rs`, `uninstall.rs`).** `verify`
  passes with every unit active and health 2xx, defers on a unit inactive or a non-2xx, and ends in `rollback` at the
  deadline; `cutover` fails when a unit is not active within its bound; a service on built-in steps with no `uninstall`
  bound is uninstalled; one with a hook program and no bound is `uninstall_unsupported`; an uninstall stopped after
  `hook_done` resumes and runs `remove` again; the previous binary started on an `uninstalling_installed` tombstone
  leaves the package and the records.
- **Rollback of hostkeeper (A6; `crates/hostkeeper-daemon/tests/selfupdate.rs`).** A rollback of `hostkeeper` to a
  release before an installer of a version the host holds is `rollback_below_installer`, and one to a release that has
  it goes on.

This list is complete in this sense: it holds one case for every refusal, outcome, alert and bound named in sections 3
to 7 and 9.1, one under and one over every limit, and every installer's four steps against crash, repeat, failure and
removal. Each item's pull request names its tests (rule T8).

## 13. Build items, one pull request each

| # | Item | After | Proof |
|---|---|---|---|
| A1 | `release.proto`; the pure crate `hostkeeper-release` with the manifest and target checks of sections 3.4 and 4.4, shared by coordinator and daemon | none | `additions_targets.rs`; `manifest.rs` at 99% lines and branches (rule T1) |
| A2 | Coordinator: `SetDesired` with targets, the 96 KiB limit for it, the gateway's null step, the audit entry, releases in the file store and in Postgres, `GetRelease`, the refusals | A1 | `targets.rs`; the previous coordinator starting on the new state |
| A3 | Coordinator: `installers` read from gRPC beats and shown; targets in gRPC orders; `target-v1` echoed; `no_target`, `target_unsupported`, `installer_unsupported` | A2 | `additions_installers.rs`; `target_orders.rs` |
| A4 | Daemon: `target-v1`, `stores.toml`, fetch and proof through the target, `hostkeeper-archive` through the target, `order_invalid`, replies up to 32 KiB, the channel path kept, the `agent.proto` header | A3 | `stores.rs`, `fetch.rs`; the self-update device tests through targets; the mixed cases of section 9.3 |
| A5 | This repository's and `hello`'s release workflows set each release with `hostkeeper-archive` targets and the release index, through the generated bash client; `channel-sync` keeps filling the channel | A2, the two-programs design's bash client | The workflow lint (`cargo test -p xtask --test workflows`) for the new steps; the next release on `main` answering `GetRelease` |
| A6 | `hostkeeper-install` with the installer trait and `tar`; the reader's gzip and xz decoders and free layout (new dependencies declared at the root, rules R5 and R6); the walk after `stage`; `VersionDir::read`; manifest-driven placement; built-in steps; the `l` tree record; `install-record.json`; `installed` and `uninstalling_installed`; `installer_changed`, `target_changed`, `rollback_below_installer` | A4 | `tar.rs`, `formats.rs`, `artifact.rs`, `builtin_steps.rs`, `uninstall.rs`, `selfupdate.rs`; a device test of `hello` as a plain tarball |
| A7 | `deb`, with the observer reading units from the tree | A6 | `deb.rs`; `install_deb.rs` in `privileged` |
| A8 | `rpm` | A6 | `rpm.rs`; `install_rpm.rs` in `privileged`, in a container with `rpm` |
| A9 | `macos-pkg` | A6 | `macos_pkg.rs`; the macOS device suite in `macos-device` |
| A10 | `oci-image`, the generated units, the observer's image digest | A6 | `oci_image.rs`; `install_oci_image.rs` in `privileged` with podman |
| A11 | Stage 5: the channel keeps only hostkeeper's archives; `channel-stage` and the other services in `channel-sync` go; their docs; hostkeeper's own workflow keeps sending its index | both conditions of stage 5 | Sets with an index still accepted; the workflow lint for hostkeeper's index; the supersession check of rule B22 |
| A12 | Stage 6: the daemon drops the channel fetch and its `Platform` copy, and requires `target-v1` echoed | A11 | The daemon's `coordinator_too_old` test; the device tests of section 9.3 |

A7 to A10 can run beside each other. The docs a behaviour change makes stale move with its item (rule T19):
`docs/package-format.md` with A6 (it becomes the `hostkeeper-archive` installer's page), `docs/hello.md` with A7,
`docs/deploy/ci-client.md` with A5, `docs/gateway.md` with A2, and hostkeeper-design.md's section 5.5 and the platform
line of its section 4.7 with A4. That file is already over 1000 lines, so each edit adds no line to it (rule R12).

**#179's items.** P1 and P2 (the vocabulary and `hostkeeper-facts`), P3 and P4 (facts on the wire and the probes) and
P9 (variant builds of `hello`) are dropped from v1 (section 11). P5 (`Register`) is replaced by A1 and A2, P6
(`Artifact` in orders) by A3, P7 (the daemon's stores and fetch) by A4, P8 (CI registering) by A5, P11 (the `oci-image`
payload) by A10, and P10 and P12 (stages 5 and 6) by A11 and A12.

## 14. Decisions

| Question | Decision | Why |
|---|---|---|
| `targets`: a map or three fields | A map keyed by the platform's spelling, the three keys documented and the only ones accepted in v1 | The JSON keys are the platforms' own spellings; a fourth platform changes no operator proto or client (section 3.2) |
| A host's platform name | From `HeartbeatRequest.platform`, the daemon's compile target, mapped by `index_platform` | Nothing the host reads at run time chooses its artifact; the spelling is today's |
| A null platform | Left out of the map; `null` in JSON dropped by a step before the gateway's parser; hosts shown `no_target`, not failed, not missing, not holding convergence | The release did not mean that platform to update, as `no_package_for_platform` today |
| A target the host's daemon cannot install | `target_unsupported` or `installer_unsupported`, listed apart, and named by `Converge` | Unlike a null target, the release meant the host to update, so CI must see it did not |
| One call or `Register` and `SetDesired` | One call: `SetDesired` gains `targets`; `GetRelease` reads back | One call deploys, as asked; a separate registration adds a state nobody acts on |
| `package_name` | Kept as today's `service`, version in `desired.version` | Renaming a shipped field is not additive (rule B8) |
| Where a manifest lives | In each platform's target, complete, not attested | As ruled. The cost is stated in section 8.2 |
| Who set which manifest | Every set with targets is an audit entry, CI's too, with each manifest's digest | The manifest decides root commands; today a CI set is only logged |
| Sizes | 24 KiB a target, 96 KiB a set with targets, 32 KiB a reply to a `target-v1` daemon | Derived from the largest manifest and locators; a reply carries one order |
| Targets on the JSON heartbeat route | Never; archive orders go out in today's form there, others wait for the next gRPC beat | The JSON route is frozen and its reply is read strictly |
| Installers built in or delivered | Built in, a closed set; a new format comes as a tarball whose hook installs it | An unattested manifest must not choose which attested program installs everything; the tarball route needs no hostkeeper release |
| How the daemon says which installers it has | `HeartbeatRequest.installers`, from the tools present at start | The set depends on the host's tools and grows with each format; the 16-name feature list would fill |
| One installer per service | Recorded at first install; another is refused until uninstall | Keeps an archive service's attested manifest from being bypassed by relabelling it `tar` |
| Installers as subprocess verbs or in-process | In-process modules running the format's tools through the step launcher | The two-programs design removes plumbing verbs; the tools still run bounded in transient units |
| What `stage` extracts | The whole package tree, with the format's own extractor, no scripts, then walked, bounded and normalised by the core | The hook can run from it before `activate`, and the observer can hash the attested files |
| An archive version's layout | Today's, with no `target.json` | An older binary reads it unchanged after a fallback |
| An order for a sealed version | The sealed `target.json` wins; a different target is `target_changed` | A release is immutable, and the daemon checks it rather than trust the coordinator |
| Prune and uninstall | Two steps, `retire` (a version's directory only) and `remove` (the service's installed package) | A native format's database holds only the installed version, so pruning an old one must not call the tool |
| `deb` and `rpm` removal | Purge or erase, only while the recorded version is installed | Removes the package's files as the format lists them; a package changed by hand is not ours to remove |
| Package scripts that restart the service | Not suppressed; the service builds packages that leave restarts to its hook | Suppressing them would mean writing `policy-rc.d` into `/usr`, host-wide, for one format only |
| A service with no hand-over | Built-in steps, chosen by an empty `command`; always uninstallable | Most packaged services and every image need only restart and check |
| An OCI image's units | Generated from a fixed template; args after the image; structured ports and volumes | An unattested manifest must not write unit text or runtime options, which are a root command line |
| Rollback of hostkeeper below an installer in use | Refused by the daemon, from a compiled table of each installer's first release | The daemon knows what its disk holds; the coordinator does not |
| An earlier binary after a fallback | Reports the services it cannot read and leaves them; never uninstalls them | A fallback cannot be refused, and must not remove what it does not understand |
| hostkeeper's own release | `hostkeeper-archive` at every stage, with its index for as long as the channel keeps it | Self-update needs `bin/hostkeeper` in a sealed tree, and every old daemon must fetch the next release |
| Host facts and variant builds | Later (section 11), as a new field beside `targets` | One target per platform needs no matching |

## 15. Questions for the maintainers

1. **Attest the manifest too.** Section 8.2 states that whoever may call `SetDesired` for a service decides root
   commands on its hosts, as ruled. CI could write each manifest as a file of the release and attest it in the same
   step as the artifacts; the daemon would verify it as it verifies the artifact, and the coordinator would forward it
   unchanged. It is additive (an optional `Blob manifest_attestation` on the target) and could follow A4.
2. **The OCI installer in v1.** A10 is designed and can wait if no service ships an image yet. The tarball route of
   section 5.1 covers an image in the meantime.
3. **The hostkeeper-only channel after stage 5.** Kept, so a host installed from an old deb or pkg can still update
   itself (rule B8). Retiring it would raise the bootstrap floor, which needs a waiver of B8.
4. **A registry whose token service is on another origin** (section 7.3). Out of v1; allowing one would need a
   `token_origin` key per store.
