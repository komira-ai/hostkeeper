# Packages for any platform: host facts, artifact kinds and registered releases

Status: design, for the maintainers' ruling. Nothing here is built. It changes three things in
[hostkeeper-design.md](hostkeeper-design.md) section 5.5 ("One package per operating system and CPU family", "The release
channel", "The coordinator reads the index too") and the platform line of its section 4.7. Where those disagree with this
document on how a host's build is chosen, how a release reaches the coordinator or where the daemon fetches bytes, this
document wins once it is built. The tags are those of hostkeeper-design.md: **Built** names code on `main` with a path, and
**Designed** means nothing implements it yet. Section 12 lists the build items, P1 to P12, one pull request each.

hostkeeper installs whatever a service ships, but three things in today's code are fitted to the three platforms this
project builds for, and to the way its own releases are laid out:

1. The platform is a closed set of three spellings, compiled into the daemon, the coordinator and the release tools.
2. A package is exactly one kind of thing: a `tar.zst` archive holding `manifest.toml`, `bin/`, `units/` and `files/`.
3. A release reaches hosts through a tree of files under one HTTPS origin that something must lay out by hand or by a
   sync job, and the coordinator learns the release from an index pasted into `SetDesired`.

This design makes each of them data. A host reports facts about itself; a release lists builds, each with the facts it
needs; the coordinator picks the build for each host; the bytes live in stores named per build; CI registers the release
through the API. The guarantees stay as they are (section 8).

## 1. What changes for the operator and for a service's CI

**Before.**

- A service is built for `linux-x86_64`, `linux-arm64` and `macos-arm64`, and for nothing else or finer. An x86-64-v3
  build, a build for macOS 15 and later, or a CUDA build can only exist inside the one package of its platform, chosen
  at run time by the package's own launcher.
- CI publishes a GitHub release. On the channel's host, `cargo xtask release channel-sync` pulls it into
  `<channel>/<service>/<version>/` (or an operator runs `channel-stage`), and CI then sets the version desired with the
  index text inline (`hostkeeper ci set-desired --release-index-file`).
- Every daemon fetches from the one `release_channel` of its config, over HTTPS, without credentials.

**After.**

- A host reports its facts in every heartbeat: `os`, `arch`, `isa_level`, `cpu.feature`, `os_version`, `libc`,
  `accelerator.*`, `runtime` and the rest of section 3. `Hosts.Get` and the UI show them.
- A release is a list of **builds**. Each names the facts it needs (`arch = arm64`, `isa_level >= armv8.2-a`,
  `os_version >= 15`, `accelerator.vendor = nvidia`), its kind, its digest and size, its attestation bundle, and where its
  bytes are. The coordinator gives each host the most specific build it satisfies, or shows the host `no_build`.
- CI registers a release with one call, `Releases.Register`, made by the generated client, then sets it desired with
  `Releases.SetDesired` naming the version. Nobody lays out a tree for a new build. Until stage 5 of section 9, CI
  also sends the legacy index and `channel-sync` keeps filling the legacy tree for old daemons. After it, the channel
  holds only hostkeeper's own legacy builds, for old bootstrap packages.
- The bytes stay where CI published them (the GitHub release's assets) and may also sit in a mirror the host is
  configured to use (a tierstore OCI front on the LAN, any OCI registry, any HTTPS origin). A daemon reads its stores
  and their credentials from `/etc/hostkeeper/stores.toml`.
- A service may ship OCI images with its package, run by the host's container runtime (section 5.3).

**What does not change.**

- An order names exact bytes by SHA-256.
- The daemon checks the size, the digest and the attestation of the archive before the first byte is decompressed or
  parsed.
- Nothing about a store, an index or a location is trusted.
- The hook contract, the step record, the switch of `current`, placement, health observation and the rollout gate.
- The three platforms hostkeeper itself is built for (`linux-x86_64`, `linux-arm64`, `macos-arm64`). This design opens
  the platforms of the services hostkeeper deploys. It keeps the daemon's own `compile_error!`, since the daemon has no
  other release target.
- Daemons already on hosts keep working at every stage of section 9 with no change.

## 2. Today, in the code

This section is what the rest of the design changes. Every item is **Built** at the path given.

**Six closed platform types and two Mac spellings.** The platform is a closed type in:

- `hostkeeper_config::platform::Platform`: `linux-x86_64`, `linux-arm64`, `macos-arm64`, with a `compile_error!` on any
  other target (`crates/hostkeeper-config/src/platform.rs`);
- its copy in `crates/hostkeeper-package/src/index.rs`, which the daemon's fetch uses;
- `hostkeeper_wire::Platform` and `hostkeeper_link::beat::Platform`, which spell the Mac `darwin-arm64`;
- the proto enum `hostkeeper.agent.v1.Platform` (`PLATFORM_DARWIN_ARM64 = 3`), sent as `HeartbeatRequest.platform = 1`;
- the release tools' `Platform` (`crates/xtask/src/release/contract.rs`).

`index_platform` in `crates/hostkeeper-coordinator/src/registry.rs` maps the wire spelling to the index spelling. The
three spellings appear on more than 900 lines under `crates/` (`grep -rnE "linux-x86_64|linux-arm64|macos-arm64" crates
| wc -l`, measured on `main`), most of them in tests and in `xtask`.

**What a host reports.** Only that enum. The daemon takes its platform from its compile target. It reads no OS version,
libc, CPU feature or GPU (`crates/hostkeeper-daemon/src/links.rs`, `observer.rs`). Enrollment's `Claims` identify a
machine and authorise nothing.

**The release index.** It is `{"packages":[{"bytes","file","platform","sha256"}],"schema":1,"service","version"}`,
canonical JSON, at most 64 KiB. Three parsers read it, all strict:

- `crates/hostkeeper-config/src/index.rs`, used by the coordinator and `channel-stage`;
- `crates/hostkeeper-package/src/index.rs`, used by the daemon;
- `crates/xtask/src/release/index.rs`.

A strict parser refuses an unknown key and a platform outside the three. It also requires `file` to be
`S-V-{platform}.tar.zst`. So one entry an old daemon cannot read makes the whole index `index_invalid` on that daemon,
even when its own platform is listed.

**The channel.** It is `{release_channel}/S/V/index.json`, the archive at `S-V-{platform}.tar.zst` and its bundle at
`.sigstore.json` beside it (`crates/hostkeeper-config/src/index.rs`). It is laid out by `cargo xtask release
channel-stage` or pulled from GitHub releases by `cargo xtask release channel-sync` (`docs/deploy/release-channel.md`).
No workflow uploads to it.

**The order.** `Update`, `Install` and `ForceRollback` carry `service = 1`, `version = 2`, `package_digest = 3` and
`index_digest = 4`. `Force` carries `service = 1`, `version = 2`, `package_digest = 3`, `halt_generation = 4` and
`index_digest = 5` (`agent.proto`). The daemon fetches the index,
checks its digest when the order has one, and takes its own platform's entry. It downloads and hashes the archive, then
fetches `<archive>.sigstore.json` and verifies it offline with the pinned `gh attestation verify` against
`[trusted_signers]`. It compares the archive's digest with the order's, and only then unpacks
(`crates/hostkeeper-package/src/fetch.rs`, `prove.rs`, `crates/hostkeeper-daemon/src/real_packages.rs`).

**The coordinator's view of a release.** `Releases.SetDesired` takes `package_digest` (one archive for every host) or
`release_index` with `release_index_digest`. The coordinator parses the index and keeps an `IndexRecord`: the index
digest and one package digest per platform, with `#[serde(deny_unknown_fields)]`. It orders each host the digest of its
platform, and skips a host whose platform is not listed with `no_package_for_platform` (`registry.rs`, `decide.rs`). The
operator API's message limit is 4 KiB (`crates/hostkeeper-coordinator/src/grpc/mod.rs`), so `hostkeeper ci set-desired`
caps an inline index at 3 KiB (`crates/hostkeeper-ci/src/flags.rs`).

**Capability negotiation.** A daemon lists features in `HeartbeatRequest.features`: `install-v1`, `forced-v1`,
`abort-v1` and others. The coordinator answers in `HeartbeatResponse.features` with those of them it supports, so
one name means "this daemon can take it" on the way in and "this coordinator will send it" on the way back. The
coordinator sends a verb only to a daemon that announced it. An order of a verb the daemon
does not know decodes as no verb and is dropped with a log line (`crates/hostkeeper-link/src/proto_map.rs`, test
`an_order_of_another_verb_or_none_is_dropped_with_why`). A field the daemon does not know is skipped by prost.

## 3. Host facts: the platform as data

### 3.1 The facts a host reports

A **fact** is a typed value the daemon reads about its own host. The keys and their spellings are jobscheduler's
(section 3.6). hostkeeper reports the keys below and no others. A key whose probe fails or does not apply is absent,
never guessed.

| Key | Kind | Linux | macOS | Today |
|---|---|---|---|---|
| `os` | text | `linux` | `macos` | the compile target only |
| `arch` | text | `x86_64` or `arm64` (`aarch64` is read as `arm64`) | `arm64` | the compile target only |
| `isa_level` | level | the highest of `x86-64-v1` to `x86-64-v4`, or of `armv8.0-a` to `armv8.4-a`, whose features are all present | the highest `armv8.N-a` from `sysctl hw.optional` | not read |
| `cpu.feature` | set_text | the `flags` (x86-64) or `Features` (arm64) line of `/proc/cpuinfo`, the intersection over all CPUs | `hw.optional` keys, with the Linux name added where one exists (`FEAT_AES` gives `aes`) | not read |
| `page_size` | count | `getconf PAGESIZE` | `getconf PAGESIZE` | not read |
| `os_version` | version | none | the product version (`15.1`) | not read |
| `distro` | text | `ID` of `/etc/os-release` (`ubuntu`, `debian`) | none | not read |
| `distro_version` | version | `VERSION_ID` of `/etc/os-release` (`24.04`) | none | not read |
| `kernel` | version | the release's leading numbers (`6.8.0` of `6.8.0-45-generic`) | the Darwin version | not read |
| `libc` | text | `glibc` or `musl` | none | not read |
| `libc_version` | version | `getconf GNU_LIBC_VERSION` for glibc | none | not read |
| `accelerator.vendor` | text | `nvidia` or `amd`, from PCI class and vendor | `apple` | not read |
| `accelerator.model` | text | the family, through jobscheduler's alias table (`nvidia-h100`) | `apple-m` | not read |
| `accelerator.count` | count | the number of devices | `1` | not read |
| `accelerator.memory-gib` | count | per device, from `nvidia-smi` | none | not read |
| `accelerator.driver` | version | `nvidia-smi --query-gpu=driver_version` | none | not read |
| `accelerator.cuda` | version | the CUDA version the driver supports, from `nvidia-smi` | none | not read |
| `gpu.metal_family` | count | none | from the chip name (M1 gives 7) | not read |
| `runtime` | set_text | the container runtimes present and answering: `podman` | none | not read |

The probes run helpers by absolute path, through a command port with a bound on time and on output. They read files
through a file port, as jobscheduler's agent does. Unit tests feed both ports fixtures and touch no system path (rule
T12). The daemon probes at start, after each order it finishes, and every 15 minutes. A failed probe leaves its keys
absent and raises the alert `facts_probe_failed` with the helper's name.

The header of `crates/hostkeeper-config/src/platform.rs` says the daemon knows its platform from the target it was
compiled for, "never from a config file or from the machine at run time". That stays true for `os` and `arch`, which
still come from the compile target. The other facts are new, and they are read from the machine.

### 3.2 Kinds and constraints

There are five kinds. Four are jobscheduler's (`text`, `version`, `set_text`, `count`, in its
`docs/design/capabilities.md`), and `level` is new here. jobscheduler's `set_version` is not adopted, because no key
here is a set of versions. The ops below are a subset of jobscheduler's.

- `text`: a token of letters, digits, `.`, `_` and `-`, at most 128 bytes.
- `version`: one to four components of decimal digits, compared as numbers after padding with zeros. Unlike
  jobscheduler's grammar, a leading zero is allowed, because distributions write `24.04`. `24.04` equals `24.4`.
- `level`: an ISA level, ordered within its family. Two families never compare.
- `set_text`: a set of tokens.
- `count`: a whole number up to 2^32.

A build's **constraint** is `{key, op, value}`:

| Kind | Ops | True when |
|---|---|---|
| text | `eq`, `in` | the host's value equals the value, or one of at most 32 values |
| version | `ge`, `lt`, `eq`, `series` | as compared; `series` matches whole leading components, so `series 15` matches `15.4.1` and not `16`, and `series 24.04` matches `24.4.1` and not `24.40` |
| level | `ge` | same family and at least the level |
| set_text | `has` | the host's set holds every listed token |
| count | `ge`, `eq` | as compared |

A host that does not report a key fails every constraint on it. A constraint on a key that is not in the table of
section 3.1 is refused when the release is registered (`entry_invalid`), because the coordinator could not evaluate it,
and skipping it would be a guess. A new key is a coordinator release, which is how jobscheduler adds one too. Values
are open. Any `os`, `arch` or `distro` text is accepted, so a build for a platform hostkeeper has never seen is
registered as data, with no change to the code.

### 3.3 The wire: one more field in the heartbeat

```proto
// agent.proto, additive
message HeartbeatRequest {
  // ... fields 1 to 7 as today; `platform = 1` is still sent by every daemon on the three platforms
  HostFacts facts = 8;          // absent from a daemon before P4
}
message HostFacts {
  map<string, FactValue> facts = 1;   // at most 64 keys of at most 64 bytes; at most 8 KiB encoded in all
}
message FactValue {
  oneof value {
    string text = 1;
    string version = 2;
    string level = 3;
    TextSet set = 4;              // at most 256 items of at most 32 bytes
    uint64 count = 5;
  }
}
message TextSet { repeated string items = 1; }
```

Every heartbeat carries the full set. The facts are at most 8 KiB encoded, leaving the rest of the heartbeat's 16 KiB
limit (`MAX_HEARTBEAT_BYTES`) for what it carries today. An x86-64 host with AVX-512 lists about 150 `cpu.feature`
names of at most 20 bytes, about 2 KiB. The coordinator keeps its fleet view in memory only (coordinator-storage.md
section 1). Sending the full set every time means a coordinator that just restarted knows every host's facts after one
heartbeat, with no revision handshake.

The daemon enforces the limits before it sends. It drops a key whose value breaks its own limit, and it drops whole keys,
largest first, until the facts fit in 8 KiB. Each dropped key raises `facts_probe_failed` naming it. So the heartbeat
still goes out, and the host is at worst `no_build` for a build that needs the dropped key. The coordinator refuses
`invalid_heartbeat` only for facts a correct daemon never sends: a value that does not parse as its key's kind, or a
limit broken anyway.

`Host` in the operator API gains `HostFacts facts = 18`, the facts the coordinator matches on: those the daemon sent,
or those read from `platform` for a daemon that predates facts (section 3.4). `Host.heartbeat` keeps only what the
daemon sent. `Hosts.Get`, the UI and the generated clients show both.

### 3.4 The three platforms are facts too

A heartbeat without `facts` comes from a daemon that predates them. The coordinator reads its `platform` as facts:

| `platform` | Facts |
|---|---|
| `PLATFORM_LINUX_X86_64` | `os = linux`, `arch = x86_64` |
| `PLATFORM_LINUX_ARM64` | `os = linux`, `arch = arm64` |
| `PLATFORM_DARWIN_ARM64` | `os = macos`, `arch = arm64` |

When both are present, `os` and `arch` must agree with `platform`, or the heartbeat is refused `invalid_heartbeat`. A
daemon on one of the three platforms keeps sending `platform`, so an older coordinator still reads it (section 9). A
heartbeat with facts and `PLATFORM_UNSPECIFIED` is accepted by a coordinator from P3 on. It is what a daemon would send
from a fourth platform, if hostkeeper is ever built for one.

### 3.5 How the cases in question are written

| Need | Build constraints | A host gets it when |
|---|---|---|
| One arm64 Linux build | `os eq linux`, `arch eq arm64` | any arm64 Linux host |
| An arm64 build that needs SVE | the above and `cpu.feature has sve` | the CPU reports `sve`; others get the plain build |
| An armv8.2-a build beside a baseline | build A: `os eq linux`, `arch eq arm64`; build B: A's and `isa_level ge armv8.2-a` | an armv8.2-a or later host gets B, an older one A |
| An x86-64-v3 build beside v1 | build A: `os eq linux`, `arch eq x86_64`; build B: A's and `isa_level ge x86-64-v3` | the same, by level |
| A 64 KiB page kernel (some arm64 servers) | `page_size eq 65536` in addition | only hosts with that page size |
| macOS 14 and later, with a separate build for 15 and later | build A: `os eq macos`, `arch eq arm64`, `os_version ge 14`; build B: A's with `os_version ge 15` | 15.1 gets B, 14.6 gets A, 13.6 gets `no_build` |
| A CUDA build beside a CPU build | build C: `os eq linux`, `arch eq x86_64`; build G: C's and `accelerator.vendor eq nvidia`, `accelerator.cuda ge 12.4` | a host with an NVIDIA driver for CUDA 12.4 or later gets G, every other x86-64 Linux host C |
| An H100-only build | `accelerator.model eq nvidia-h100` in addition | only H100 hosts |
| A musl build | `libc eq musl` | only musl hosts; a glibc host gets nothing from it |
| A build for Ubuntu 24.04 only | `distro eq ubuntu`, `distro_version series 24.04` | 24.04.x |

Inside one build, choosing among instruction-set variants at run time (glibc-hwcaps, a launcher) stays the build
system's business, as hostkeeper-design.md section 5.5 says. A service that ships one build per level registers them as
separate builds instead.

### 3.6 One vocabulary with jobscheduler

jobscheduler matches jobs to hosts with the same idea: an agent reports typed facts, and a request states constraints
over them. That is `crates/js-caps` and `js-types` (the `Arch` spelling with `aarch64` read as `arm64`, the ISA ladders
`X86Level` and `ArmVersion`, the accelerator alias table), and its design `docs/design/capabilities.md` (the five kinds
and the version grammar). hostkeeper adopts that vocabulary: the same keys, the same spellings, the same kinds but for
`level`, the same version order and the same ISA ladders. A fact means the same thing in both projects, and an operator writes
`arch = arm64` the same way in both.

Two differences:

1. **The code is not shared.** `deny.toml` allows no git source (`allow-git = []`), and jobscheduler's crates are
   `publish = false`. So hostkeeper implements the keys in its own pure crate, `hostkeeper-facts`. The shared part is the
   vocabulary as data: a key table with each key's kind and spellings, plus test vectors (cpuinfo, sysctl and
   `nvidia-smi` fixtures with the facts they must give, and constraints with their verdicts). jobscheduler holds the
   source. hostkeeper checks in a copy with its SHA-256, and a test fails when the copy and the hash disagree. That is
   how this repository already keeps the shared core of `AGENTS.md` (`crates/xtask/tests/agents_md.rs`). A change to the
   vocabulary is a pull request in jobscheduler, then a pull request here that moves the copy.
2. **Keys jobscheduler does not have yet.** These are `distro`, `distro_version`, `kernel`, `libc`, `libc_version`,
   `accelerator.driver`, `accelerator.cuda`, `runtime` as a set, and the `level` kind for `isa_level` (jobscheduler
   compares it within its family but has no name for the kind), and leading zeros in a version. P1 proposes them to
   jobscheduler first. Until
   jobscheduler takes them, they are hostkeeper's, and the copy marks them so.

jobscheduler's `--cap key=value` override and its image profiles are not adopted. A hostkeeper fact is what the daemon
reads. An operator who wants a host to get a different build changes the build's constraints, or excludes the host.

## 4. Builds and how the coordinator picks one

### 4.1 A registered release

```proto
// operator.proto, additive
message ReleaseBuild {
  string build = 1;                  // a name, unique in the release: [a-z0-9][a-z0-9._-]{0,63}, e.g. "linux-x86_64-v3"
  string kind = 2;                   // "archive" in v1 (section 5)
  string digest = 3;                 // SHA-256 of the artifact, 64 lower-case hex
  uint64 bytes = 4;                  // its size
  repeated Constraint needs = 5;     // all must hold (section 3.2); at most 32
  Blob attestation = 6;              // the Sigstore bundle of the artifact, itself named by digest
  repeated Location locations = 7;   // where the artifact's bytes are, in preference order; at least one
  repeated Payload payloads = 8;     // section 5.3; empty for a plain archive
  optional string legacy_platform = 9; // "linux-x86_64", "linux-arm64" or "macos-arm64": this build is the one
                                       // the release's legacy index lists for that platform (section 9)
}
message Constraint { string key = 1; string op = 2; repeated string values = 3; }
message Blob { string digest = 1; uint64 bytes = 2; repeated Location locations = 3; }
message Location { string store = 1; string locator = 2; }   // section 7
message Payload { string name = 1; string kind = 2; Blob blob = 3; }
```

The build's name is what the archive's manifest names in its `platform` key (section 5.2). For the three legacy builds
it is the legacy spelling, so an archive built today is a valid build as it is. Every other build also states its
constraints in its own attested manifest (section 5.2), so the bytes, not only the coordinator, say where they may run.

### 4.2 Most specific wins, and no guess

For each host and each desired release, the coordinator takes the builds whose constraints all hold on the host's facts
and whose kind (and every payload's kind) the host's daemon announced (section 5.1). A daemon that does not announce
`artifact-v1` is matched to legacy builds only, those whose `legacy_platform` is its own platform (section 9.1). Then:

- **None:** the host has no build. It is ordered nothing for update, install or force, and shows the gate reason
  `no_build`, with the constraint each build failed on as `detail`. It is never counted as failed, and it is not
  counted toward convergence either: `Releases.Status` and the `converge` answer list such hosts apart, as they list
  `no_package_for_platform` hosts today. This is today's `no_package_for_platform`, generalised.
- **One:** that build.
- **Several:** the host gets the matching build that is more specific than every other matching build. If there is no
  such build, the host shows `ambiguous_build`, naming the builds, and is ordered nothing. That happens when two builds
  match and neither is more specific, for example a v3 build and a CUDA build on a v3 host with a GPU. The coordinator
  never picks one by order, by name or by count of constraints.

**More specific.** For each key, the constraints of one build on that key reduce to one **allowed region** of values:

- a text key: a set of tokens (`eq` is one token, `in` several, and two constraints on one key intersect);
- a version or count key: an interval (`ge` and `lt` bound it, `eq` is a single point, and `series 15` is
  `[15, 16)`);
- a level key: a family and a lowest level (levels of different families never meet);
- a set key: the tokens the host's set must hold (the union of the `has` lists).

A key the build does not constrain allows everything. Build A is more specific than build B when, on every key, A's
region lies inside B's, and on at least one key it is strictly smaller. Containment of intervals, token sets and
ladders is exact, so the relation needs no search over hosts. For a set key, "inside" means A requires every token B
requires.

**Registration refuses two builds whose regions are equal on every key** (`entry_invalid`), however their constraints
are written: `eq linux` and `in linux` are the same region, and so are `series 15` and `ge 15` with `lt 16`. That is
the one ambiguity it can see without a host. It cannot see the others in advance, since they depend on the fleet's
facts. The fix is to register a build that is more specific than both (v3 and CUDA), or to narrow one of them.

**Rollback is not matched again.** Automatic rollback and `ForceRollback` go back to the build the host ran before,
which the coordinator recorded when it ordered that build (section 6.4). Their target is already sealed on the host, so
they need no fetch. A host whose facts changed since (a GPU removed, or a probe that failed) can always go back.

The choice is a pure function of the facts, the builds and the announced features. It lives in `hostkeeper-facts` and
is tested by table (section 11). A host whose facts change between heartbeats is matched again at the next decision.
An order already sent is not withdrawn.

### 4.3 The daemon checks the build is its own

Today the daemon picks its own entry from the index rather than trust the coordinator's choice of platform. Now the
coordinator chooses, and the daemon takes two looks:

1. **Before the fetch,** it checks the order's constraints (section 7.1) against its latest facts. A build its host does
   not satisfy is `Deferred`, `environment`, `build_unsuitable`, and the next heartbeat carries the facts that changed.
   This catches a coordinator with stale facts, and costs no download.
2. **After verification,** it checks the constraints the attested manifest states (section 5.2) against the same facts.
   These come from the bytes the trusted signer built, not from the coordinator. If they fail while the order's
   constraints held, the registration and the package disagree: `Failed`, `release`, `needs_mismatch`.

Neither look applies to a rollback, whose target is already on the host.

## 5. Artifact kinds

### 5.1 The contract a kind implements

A **kind** is the code that turns verified bytes into a sealed version directory, and later removes them. It is
compiled into the daemon. It is not a plugin loaded at run time, because the daemon runs no code it did not ship with or
that an attested package did not bring. Each kind has a name (`archive`, `oci-image`) and a daemon feature
(`kind-archive-v1`, `kind-oci-image-v1`). The coordinator matches a build to a host only if the host announced the
build's kind and every payload's kind.

What the core does, the same for every kind, in this order:

1. Fetch the artifact's bytes from a store by digest into a staging file, counting and hashing them as they stream
   (section 7). Refuse a size or digest that differs from the order's.
2. Fetch the attestation bundle by its digest, and verify it against `[trusted_signers]` with the pinned verifier, as
   `prove.rs` does today. The bundle must list the artifact's digest.
3. Hand the kind only a `VerifiedArchive`, the type `prove.rs` already makes impossible to build any other way.

What a kind implements (a trait in `hostkeeper-package`, **Designed**):

| Method | Does | Bounds and refusals |
|---|---|---|
| `closure(&verified)` | Lists the further blobs the artifact names by digest (an image's config and layers), read from the verified bytes. A plain archive names none | At most 128 blobs and `max_package_bytes` in total. The core fetches each by digest and checks it as in step 1 |
| `stage(&verified, &blobs, &mut StageDir)` | Writes the version's tree into the staging directory: `manifest.toml` and whatever the kind lays out, regular files and directories only | The archive's bounds (section 5.5 of hostkeeper-design.md) apply to every kind. The core then reads the manifest, seals the tree and publishes it, as `publish.rs` does |
| `load(&version_dir)` | Makes the sealed version usable by the host before `prepare`, for example importing an image into the runtime's store. It adds to the host and replaces nothing that runs: what runs changes only at `cutover`, as today. Idempotent; run again at daemon start for `current` and `previous` | Bounded by `[hook.timeout_secs] prepare`, or 300 s when that is 0. Failure in an order is `Deferred`, `environment`, `load_failed`. At daemon start a failure raises `load_failed` and never blocks the start; the next step's `load` tries again |
| `unload(&version_dir)` | Undoes `load` when the prune removes the version, but leaves anything another kept version of the service (`current`, `previous` or one being staged) also loaded, such as an image of the same digest | Failure leaves the version for the next prune and raises `unload_failed` |

The kind never touches the hook, the step record, the switch of `current` or placement. A version is still a sealed
directory with a manifest, so `Staged::check`, which re-hashes the tree before every step, covers every kind with no
change.

### 5.2 `archive`: today's package, the first kind

This is today's `tar.zst`, unpacked by `hostkeeper-unpack` with today's bounds and shape rules, and `closure` is empty.
One rule changes, and only for an order that names its build (section 7.1). The manifest's `platform` must then equal
the order's build name, instead of the daemon's compiled platform. For the three legacy builds the two are the same
string. The manifest's schema stays 1. The key keeps its name, and its value may now be any build name, which an older
daemon never sees (section 9).

A build that is not a legacy build also states its constraints in its manifest, in the key `needs`, written as the
build's constraints are registered:

```toml
platform = "linux-x86_64-cuda12"
needs = [
  { key = "os", op = "eq", values = ["linux"] },
  { key = "arch", op = "eq", values = ["x86_64"] },
  { key = "accelerator.cuda", op = "ge", values = ["12.4"] },
]
```

The daemon checks these after verification (section 4.3, second look). CI writes the same list into the manifest and
into the registration. A legacy build has no `needs`, because an older daemon would refuse the key.

### 5.3 `oci-image`: an image run by the host's runtime

v1 needs one more kind. Services that ship as images, and the stack this project already runs in containers (the
coordinator and `postgres:16`, coordinator-storage.md section 9), should be installed and updated like any other
service. An image alone has no lifecycle: it has no hook, no units, no health URL. So in v1 an image is a **payload** of
an archive, not a build of its own:

```toml
# manifest.toml of a service that runs an image; schema stays 1
[[payloads]]
name   = "server"
kind   = "oci-image"
digest = "<64 hex>"             # the image manifest's SHA-256, bare hex as every digest here; OCI's sha256:<hex> is formed by the daemon
```

- **Trust.** The archive is attested, and its attested manifest names the image by digest. An image is trusted because
  an attested package names its exact bytes, which is the same reason `files/` are trusted. This also covers an image
  this project did not build (`postgres:16`, pinned by digest): the service's CI vouches for the digest by naming it.
  No image is ever run because a registry or a tag says so.
- **Fetch.** The coordinator's order lists each payload's locations (section 7.1). The daemon fetches the image manifest
  by digest, then its config and layers by the digests the manifest lists, each checked as it streams. The manifest it
  fetched must have the digest the attested `manifest.toml` names; otherwise the result is `Failed`, `release`,
  `payload_mismatch`.
- **Stage.** The blobs are written as an OCI image layout under `payloads/<name>/` in the version directory (`oci-layout`,
  `index.json`, `blobs/sha256/<hex>`), uncompressed as fetched, and sealed with the rest of the tree.
- **Load.** The image is imported from the sealed layout into the runtime's store, under the reference
  `localhost/hostkeeper-<service>-<name>@sha256:<hex>`. P11 settles the runtime's exact commands, with a device test
  on a hosted runner. A unit in `units/` runs the image by that reference, with the runtime's flag that forbids a pull,
  so the runtime never fetches anything itself. The hook does the hand-over in `cutover` as it does for a process.
- **Runtime.** In v1 this is `podman` on Linux, rootful, run by systemd units. Daemonless, it fits units that systemd
  starts and stops. CI writes `runtime has podman` into the constraints of every build with an `oci-image` payload,
  in the registration and in the manifest's `needs`. `Register` refuses such a build without it (`entry_invalid`), so
  the stored build is exactly the request and a host without podman gets `no_build`, not a failed load. macOS has no `oci-image` in v1.
- **Health.** A unit that runs the runtime is observed by its unit state and its `[health]` URL. The process identity
  check, which compares a unit's main process with a package file (`observer.rs`), names the runtime's binary for such
  a unit, and the observer reports the image digest as the running version. P11 builds that.

### 5.4 Not in v1

| Kind | Why not now |
|---|---|
| `.deb`, `.rpm`, `.pkg` installed by the OS package manager | Their maintainer scripts run as root, outside the hook's bounds, and the manager owns paths that placement would also claim. hostkeeper itself ships a deb, an rpm and a pkg to bootstrap a host, and that is all they are for |
| A bare file (model weights, a data set) | No service needs one yet. It would be a payload kind `file`, staged as `payloads/<name>`, and it fits the contract unchanged |
| An OCI image as a build of its own, with the lifecycle in image labels | It would need a second manifest format and a hook run inside the container. An archive that carries the image does the same with what exists |
| WebAssembly or VM images | No service needs them |

### 5.5 What no kind may do

- Read or parse a byte before the core has checked its digest. For the top artifact, the attestation is also checked
  first.
- Run anything from the artifact before `load`, and run anything in `load` but the runtime's import of verified,
  sealed files.
- Fetch on its own, follow a reference the core did not check by digest, or let the runtime pull.
- Write outside the staging directory in `stage`, or outside the runtime's store in `load`.
- Change the seal, the tree digest or the meaning of a manifest key. A kind's new keys are new keys of the manifest's
  schema 1, refused by an older daemon, and only ever sent to a newer one (section 9).

## 6. Registering a release through the API

### 6.1 `Releases.Register`

```proto
// operator.proto, additive
service Releases {
  // ... today's methods
  rpc Register(RegisterRequest) returns (RegisterResponse);
  rpc GetRelease(GetReleaseRequest) returns (GetReleaseResponse);
}
message RegisterRequest {
  string service = 1;
  string version = 2;
  repeated ReleaseBuild builds = 3;          // at least 1, at most 64
  optional string legacy_index = 4;          // the canonical index.json of the legacy builds (section 9)
  optional string legacy_index_digest = 5;
}
message RegisterResponse { bool created = 1; }   // false: the same release was registered before
message GetReleaseRequest { string service = 1; string version = 2; }
message GetReleaseResponse { repeated ReleaseBuild builds = 1; optional string legacy_index_digest = 2; }
```

`Register` records a version with its builds. It does not make the version desired. It takes CI's OIDC token, for the
services the token's allow-list entry names, or the operator token, exactly as `SetDesired` does. It is one audit entry,
`release_register`. Its message limit is 64 KiB, on the gRPC route and the JSON gateway alike, because a release of many
builds does not fit in the 4 KiB every other operator method keeps.

### 6.2 What the coordinator checks

| Check | Refusal (`ReleasesRefusal`) |
|---|---|
| Service and version grammars; 1 to 64 builds; unique build names; every digest 64 lower-case hex; `bytes` from 1 up; at least one location; payload names unique within a build | `ENTRY_INVALID = 23` |
| Every constraint's key is in the vocabulary and its op fits the key's kind; values parse; no two builds whose allowed regions are equal on every key (section 4.2); a build with an `oci-image` payload has `runtime has podman` | `ENTRY_INVALID` |
| Kinds and payload kinds known to this coordinator | `ENTRY_INVALID` |
| Every location names a store kind and a locator of the grammar of section 7.1 | `ENTRY_INVALID` |
| At most one build per `legacy_platform`; a legacy build has `kind = archive`, no payloads and exactly the constraints of section 3.4 for its platform | `ENTRY_INVALID` |
| With `legacy_index`: its SHA-256 is `legacy_index_digest`; it parses (today's strict parser); it lists exactly the legacy builds, by platform, size and digest | `INDEX_DIGEST_MISMATCH = 21`, `INDEX_INVALID = 22` (today's values) |
| The same service and version registered before with other builds | `RELEASE_EXISTS = 24`, not retryable |

A release is immutable. Registering the same release again, compared field for field, answers `created = false` and
changes nothing, so a CI retry is safe. A build added later is a new version. The coordinator does not verify
attestations or fetch bytes: the daemon is the one verifier, as today, and a bad digest or bundle costs the first host a
`Failed` with class `release`, the same as a bad index entry costs today.

### 6.3 Desired state and force over a registered release

- `SetDesired` with neither `package_digest` nor `release_index` sets a registered version. That is today's third form,
  which orders nothing for a version CI never gave digests (`no_digest`). It now orders the version's builds. A
  `SetDesired` naming a version that is neither registered nor given digests stays `no_digest`, as today.
- `SetDesired` with `package_digest` or `release_index` keeps today's meaning, and is never refused for using them
  (rule B8). One check is added, and it applies only to versions registered through `Register`, which no set before
  P5 could name: with `release_index` for such a version, the index must be the release's legacy index
  (`legacy_index_digest`), or the set is refused `index_digest_mismatch`. The registered builds are then ordered, and
  the index is kept as today. That is the form CI uses until stage 5 (section 6.5): a coordinator rolled back to one
  before P5 reads the same set as today's and keeps ordering the legacy builds.
- `Force` to a registered version uses its builds, matched as in section 4.2. `ForceRollback` and automatic rollback use
  the build the host ran before, from the record of section 6.4.
- `Releases.Status` and `Hosts.Get` name each host's build, or the reason it has none.

### 6.4 Where the coordinator keeps it

A release record is new state, kept beside the registry, not inside it. `IndexRecord` and the state file refuse unknown
fields (`deny_unknown_fields`), so a new key in either would stop an older coordinator from starting after a rollback.
So:

- With the file store, releases live in a file of their own, `<state-file>.releases`. Each release is written once,
  with temp file, `fsync` and rename. An older coordinator does not read the file.
- With the Postgres store, they live in a table `releases (service, version, builds jsonb, legacy_index_digest)`, added
  by its own migration marked `may-run` (coordinator-storage.md section 6), so an older coordinator starts on the schema.
- The registry's cap of 4096 versions a service, and its eviction, apply to releases too.

The build each host was ordered is new state too, for rollback (section 4.2), `Releases.Status` and `Hosts.Get`. It is
one row per host and service, `(host, service, version, build)`, written when the coordinator first orders that build to
the host. It lives in the same file or in a second table of the same migration, never as a key in the operations file,
the force record or the state file. A row that is missing, for example from before P5, is read as today's choice: the
legacy build of the host's platform. P5 has a test that the previous coordinator starts on the files and the schema
this one writes.

### 6.5 CI publishes with the generated client

The service's release workflow, after its GitHub release is published:

1. Builds the request from the release's assets. For each archive (and image), it writes the build name, kind, digest,
   size, constraints, the bundle's digest and size, and the locations.
2. Calls `Releases.Register`, then `Releases.SetDesired` naming the version, with the generated bash client of the
   two-programs design (section 10). Until stage 5, `SetDesired` also carries the release's legacy index, so a
   coordinator rolled back to one before P5 still orders it (section 6.3). It gets a fresh OIDC token for each call, as
   `set-desired.yml` does today.

Nothing is uploaded except what the workflow already publishes, so no new upload path exists. The GitHub release is the
store of record. A mirror on the LAN fills itself by pulling (section 7.3), never by a push from CI. The constraints of
each build live in the service's repository, next to the build that produces it. This repository's own release keeps
its three legacy builds and its legacy index after stage 5 too, for the daemons older bootstrap packages install
(section 9.2).

## 7. Stores: where the bytes are and how the daemon fetches them

### 7.1 What the order carries

```proto
// agent.proto, additive
message Update        { /* 1 to 4 as today */ Artifact artifact = 5; }
message Install       { /* 1 to 4 as today */ Artifact artifact = 5; }
message ForceRollback { /* 1 to 4 as today */ Artifact artifact = 5; }
message Force         { /* 1 to 5 as today */ Artifact artifact = 6; }

message Artifact {
  string build = 1;                       // the build's name; the manifest's `platform` must equal it
  string kind = 2;
  string digest = 3;                      // equals package_digest
  uint64 bytes = 4;
  repeated FactNeed needs = 5;            // the build's constraints, for the check of section 4.3
  ArtifactBlob attestation = 6;
  repeated ArtifactLocation locations = 7;
  repeated ArtifactPayload payloads = 8;
}
message FactNeed { string key = 1; string op = 2; repeated string values = 3; }
message ArtifactBlob { string digest = 1; uint64 bytes = 2; repeated ArtifactLocation locations = 3; }
message ArtifactLocation { string store = 1; string locator = 2; }
message ArtifactPayload { string name = 1; string kind = 2; ArtifactBlob blob = 3; }
```

The coordinator fills `artifact` only for a daemon that announced `artifact-v1`, and `package_digest` always. The header
of `agent.proto` says an order is "never a command, a path, a URL or a script". That becomes: never a command, a host
path, an absolute URL, a credential or a script. A location names a store the host's own configuration defines, and a
locator relative to it. A locator is 1 to 1024 bytes of URL path characters. It has no scheme, no `//`, no query, no
fragment, no `%` and no `.` or `..` segment, and the daemon refuses any other as `NOT_READ`, as it does any field it cannot read.

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

The stores live in a new file, not in `hostkeeper.toml`. `hostkeeper.toml` refuses unknown keys, and the previous binary
reads it after a self-update falls back, so a new key there would stop that binary from starting. A daemon with no
`stores.toml` has one store, named `channel`, of kind `https`, at its `release_channel`, with no redirects. That is
today's fetch, so a daemon of P7 needs no new file to keep working.

The file is configuration, read at start. A store with an unknown kind, a base that is not `https://`, userinfo in the
base, a credential file that is not root-owned and 0600, both credential keys on one store, an `oci` mirror with no
`repository`, or two stores of one name refuses the start (rule S2:
configuration in files, not environment). Like `hostkeeper.toml`, it is part of the boot image, or written by the
operator.

### 7.3 Store kinds

| Kind | GET for a blob | Fits |
|---|---|---|
| `https` | `<base><locator>`, following up to `redirects` redirects, each to `https://` | GitHub release assets (`<owner>/<repo>/releases/download/v<V>/<file>`, which redirect to the asset host), today's channel tree (`<S>/<V>/<file>`), any static origin |
| `oci` | `<base>v2/<locator>/blobs/sha256:<hex>`, the distribution API. The locator is a repository name; a mirror store uses its `repository` and needs no locator | tierstore's OCI front (anonymous, or Basic with a read-only key on its `blobs/sha256/` prefix), any OCI registry. The GitHub release's archives and bundles are reached through `https`, images through `oci` |

S3 with SigV4 is not in v1. tierstore's OCI front serves the same bytes by digest with no SigV4 client in the daemon,
and a presigned URL would put a credential in an order. A LAN mirror fills by pulling: a sync job on the mirror's side
reads `Releases.GetRelease` and copies what is registered into tierstore (tierstore's pull-through of an upstream
registry is planned there, not built). The sync job is the mirror operator's, outside this design's build items. The
mirror is never filled by a push from CI.

### 7.4 How the daemon authenticates

- **To a store:** with the credential file of that store in `stores.toml` (`bearer_file`, or `basic_auth_file` for
  tierstore), or anonymously. The credential is the host's own, read-only and scoped to blobs. The coordinator never
  holds or sends one, and an order never carries one. A credential is sent only to its own store's base origin, and is
  dropped on a redirect to another origin.
- **An OCI registry's token challenge:** a 401 with a `Bearer realm=...` challenge is answered only when the realm's
  origin is the store's base origin. The daemon fetches a token there, with the store's credential or anonymously, and
  uses it for that store alone. A realm on another origin fails that source as `fetch_failed`. This is enough for
  tierstore, which takes Basic directly. A registry whose token service is on another origin is out of v1 (section
  14).
- **To the coordinator:** mutual TLS with the host's enrolled certificate, unchanged. The locations come from there.
- **Of the store, to the daemon:** TLS with the host's CA bundle, as today's channel fetch does (`net.rs`). That
  authenticates nothing that matters: the bytes are trusted only once the digest and the attestation check out.

### 7.5 The fetch, in order

For the artifact, its bundle, and each payload blob, the daemon tries these sources in order. First come its `mirror`
stores, in file order. Then come the order's locations, in order, skipping any whose store it does not define. It takes
the first source whose bytes have the expected size and SHA-256.

| What happened | Outcome |
|---|---|
| Some source gave the right bytes | Go on. A source that gave other bytes first raises `store_digest_mismatch`, naming the store |
| No source is defined on the host | `Failed`, `environment`, `no_store` |
| Every source failed to answer (connection, status, timeout) | `Deferred`, `environment`, `fetch_failed`, as today |
| Every source that answered gave other bytes, and one of them was a location of the order | `Failed`, `release`, `package_digest_mismatch`, as today |
| Only mirrors answered, all with other bytes | `Deferred`, `environment`, `store_digest_mismatch`: a stale or wrong mirror is the host's problem, not the release's |
| More bytes than the order's `bytes`, or than `max_package_bytes` (for a bundle, its `bytes` or 1 MiB, today's cap) | Stop the transfer; that source counts as giving other bytes |

Each source gets its own `fetch_timeout_secs` (and `attestation_timeout_secs` for a bundle), so a mirror that hangs
costs one budget and the next source is tried. Today there is one source, so today's bound is one budget. No source and no
locator is trusted. A store can make a fetch fail, never make the wrong bytes run.

## 8. The guarantees, and what keeps each

| Guarantee | Today | After |
|---|---|---|
| An order names exact bytes | `package_digest` | `package_digest`, and `artifact.digest` equal to it (a daemon refuses an order where they differ, `NOT_READ`); each payload by digest |
| Digest and attestation before any unpack | `fetch.rs`, then `prove.rs`, then `VerifiedArchive` to `unpack` | The same core, for every kind (section 5.1); a payload's digest before it is written; a payload is trusted through the attested manifest that names it |
| The index is a lookup, not a trust anchor | The daemon checks the index digest from the order, and trusts only the archive's attestation | A daemon of P7 or later, given an `artifact`, reads no index. The order's locations are lookups, equally untrusted: a wrong location or store costs a fetch, never a run |
| Only the trusted signers' builds run | `[trusted_signers]`, the pinned `gh`, the fixed issuer and predicate | Unchanged. A kind cannot add a signer or a verifier |
| A host runs only a build meant for it | The daemon takes its own platform's entry | The coordinator matches facts (section 4.2). The daemon checks the order's constraints before the fetch, and the attested manifest's `needs` after it (section 4.3). The manifest's `platform` must equal the build's name |
| No downgrade on the host | None; the floor is the coordinator's `min_version` | Unchanged |

A coordinator compromise gives what it gives today: it can order any attested build of a service to a host. It now also
picks the locations, and that adds nothing, because a location only says where to look.

## 9. Compatibility and the staged path

### 9.1 What an old daemon reads, and what it must keep getting

A daemon before stage 3 (the daemons on hosts now):

- sends `platform = 1..3` and no facts (facts come with P4), and announces no `artifact-v1` (that comes with P7);
- fetches `{release_channel}/S/V/index.json`, which must stay strict-readable: four keys, the three platforms, one
  entry each, `file` equal to `S-V-{platform}.tar.zst`;
- refuses a manifest with an unknown key (`needs`, `[[payloads]]`) or a `platform` outside the three. That holds for
  an order, and also for a version already on disk: it reads the manifests of `current` and `previous` of every service
  at start, in the observer, in force and in uninstall;
- skips the `artifact` field of an order (prost skips unknown fields).

So an old daemon is only ever matched to a build with a `legacy_platform` equal to its own. It is ordered as today: the
legacy build's `package_digest`, and the release's `legacy_index_digest` as `index_digest`. The channel tree goes on
holding the legacy index and the legacy archives while such daemons remain (stage 5). A release with no legacy build
for an old daemon's platform shows that host `no_build`, with `detail` "daemon does not announce artifact-v1".

**An old binary on a new host's disk.** A host on P7 or later may hold versions an old binary cannot read: a variant
build, or a build with payloads. An old binary runs there in two cases. One is a fallback after a self-update fails,
which runs the binary before it. That binary is P7 or later once the host has run P7 long enough to be ordered such a
build, because the coordinator orders them only to a daemon that announces `artifact-v1`. The other is a rollback of
the service `hostkeeper` itself to a release before P7, automatic or by `ForceRollback`. The coordinator refuses that
rollback while the host holds a version that is not a legacy build, and shows the hold `rollback_below_artifact`. An
operator who must go back uninstalls those services first. P7 has a test that starts the previous release's binary on
a host holding such versions. It asserts that binary's outcome: those services are reported with an unreadable
manifest, and the daemon and every legacy-build service run on unaffected.

### 9.2 The stages

Each stage is one or more build items (section 12), released in order. The coordinator goes first at every stage. A
later stage starts only when the previous one is on every host, as `Hosts.List` shows (each host's `agent.version` and
`features`).

| Stage | Released | Old behaviour still answered | Dropped | Safe because |
|---|---|---|---|---|
| 1 | Coordinator: facts read, `Register`, releases and host builds kept, matching with the legacy fallback, `artifact` in orders, `artifact-v1` and the kind features supported (P2, P3, P5, P6) | Every heartbeat without facts; `SetDesired` with an index or a digest; old daemons ordered exactly as today | Nothing | Every change is additive. No daemon announces `artifact-v1` yet, so no order carries `artifact` |
| 2 | CI registers every release, and sets it desired with its legacy index too (P8) | The channel and `channel-sync` keep laying out the legacy tree; old CI pins keep calling `SetDesired` with the index | Nothing | The registered legacy builds are the same bytes the index lists, and `Register` checks that they agree. A coordinator rolled back before P5 reads the set as today's (section 6.3) |
| 3 | Daemon: facts, `artifact-v1`, `kind-archive-v1`, `stores.toml`, fetch by digest (P4, P7) | An order without `artifact` (from an older coordinator, or for a set of an unregistered version) is fetched through the channel as today | Nothing | The old daemon updates to it through the legacy path. The new one is sent `artifact` only once it announces `artifact-v1` and the coordinator echoes it |
| 4 | Builds beyond the legacy three; `oci-image` payloads (P9, P11) | Legacy builds keep being registered for old daemons | Nothing | A new build is matched only to a daemon that announced its kinds |
| 5 | Retire the channel for every service but `hostkeeper` (P10) | `SetDesired` with an index or a digest is still accepted, never refused (rule B8). The channel keeps hostkeeper's own legacy builds, so a host installed from an older deb or pkg can still update itself | The legacy index and legacy builds in every other service's releases; `channel-stage`; every service but `hostkeeper` in `channel-sync`'s trust list; the coordinator's `--release-channel` check | Two conditions, both shown in P10's pull request. Every host announces `artifact-v1`. Each service whose channel tree goes has its last three releases registered (`Releases.GetRelease` answers them), so no CI of that service still depends on the channel. After that, a set of an unregistered version of such a service is still accepted and orders as today, without `artifact`. Its hosts then try the channel, which no longer holds the service, and stay `Deferred`, `fetch_failed`, until the version is registered |
| 6 | Daemon: drop the legacy fetch and the `Platform` copy in `hostkeeper-package`'s index (P12); `hostkeeper_config::platform::Platform` and its `compile_error!` stay | The channel of hostkeeper's legacy builds stays, for old bootstrap packages | The daemon's index parser and its `Platform` copy | The daemon requires `artifact-v1` in `HeartbeatResponse.features`. Every coordinator from stage 1 lists it as supported, so it echoes it to a daemon that announces it. Against an older coordinator the new daemon's orders are `Deferred`, `environment`, `coordinator_too_old`. The proto keeps `platform = 1` and `index_digest`, never removed (rule B8) |

### 9.3 Self-update at every stage

hostkeeper updates itself as the service `hostkeeper`, through the same order (`selfupdate.rs`). At each stage:

- **Stages 1 and 2:** daemons are unchanged.
- **Stage 3:** an old daemon fetches the new release's legacy build through the channel, probes it with
  `self-update-protocol`, switches, and restarts into it. The protocol does not change. If the new binary fails to
  start, the fallback runs the old binary. That binary reads `hostkeeper.toml`, which is unchanged because the stores
  are in `stores.toml`, and the step record. An `artifact` order adds no field to the step record
  (`crates/hostkeeper-steps/src/record.rs` keeps digests and versions, which stay as they are). The build name is kept
  by the coordinator (section 6.4). P7 checks this with the record reader of the previous release.
- **Stage 4:** hostkeeper's own release keeps its three legacy builds. A variant build of hostkeeper itself is not
  planned.
- **Stages 5 and 6:** hostkeeper's legacy builds stay in the channel and in its registered releases, so every daemon on
  a host, and every daemon an older bootstrap package installs, can update.

Each stage is proven by the existing device tests, which run on every pull request in `pr-device.yml` (the jobs
`privileged` and `macos-device`):

- `a_daemon_the_deb_installed_is_installed_its_next_release_and_restarts_into_it` (`self_update.rs`) and
  `a_daemon_the_pkg_installed_is_installed_its_next_release_and_restarts_into_it` (`device_mac_selfupdate.rs`), run
  with the next release served through the registered path from stage 3 on;
- `a_next_binary_that_cannot_start_spends_the_start_limit_and_the_fallback_puts_the_old_one_back` (`self_update.rs`)
  for the fallback;
- the `first_boot.rs` scenario for enrollment and beats with facts.

P7 adds two mixed cases: the previous release's binary as the old daemon, updated by a stage-3 coordinator to the new
binary and back by the fallback; and the previous binary started on a host holding a variant build (section 9.1).

### 9.4 Proto compatibility and the baseline

Every proto change here is additive (rule B8; the "Extending" rules of `agent.proto` and `operator.proto`):

- `agent.proto`: `HeartbeatRequest.facts = 8`; the messages `HostFacts`, `FactValue`, `TextSet`, `Artifact`,
  `FactNeed`, `ArtifactBlob`, `ArtifactLocation`, `ArtifactPayload`; `artifact = 5` on `Update`, `Install` and
  `ForceRollback`, and `artifact = 6` on `Force`.
- `operator.proto`: the RPCs `Releases.Register` and `Releases.GetRelease` and their messages; `Host.facts = 18`;
  `ReleasesRefusal` values `ENTRY_INVALID = 23` and `RELEASE_EXISTS = 24`; `GateReason` values `GATE_REASON_NO_BUILD = 31`,
  `GATE_REASON_AMBIGUOUS_BUILD = 32` and `GATE_REASON_ROLLBACK_BELOW_ARTIFACT = 33`. `GATE_REASON_NO_PACKAGE_FOR_PLATFORM = 24` keeps its meaning, for sets made
  with an index, and is not widened.

No field is renumbered, renamed, retyped or removed, and `platform = 1` stays required of a daemon on the three
platforms. The baseline `crates/hostkeeper-proto/proto/compat/baseline.binpb` does not change in any pull request
(writing it is the release's step, `compat.rs`). `the_protos_keep_every_rule_against_the_baseline` passes on each item
unchanged. Each item that adds proto also adds an `additions_*.rs` test (`additions_facts.rs`, `additions_register.rs`,
`additions_artifact.rs`). It shows that a reader built from the baseline, standing for the previous release, keeps the
new fields and writes them back unchanged, and that the new reader takes the old messages. Those are the B8 tests in
both directions for the wire. For the coordinator's state, P5 adds a test that starts the previous coordinator's reader
on a state that has releases (section 6.4). The JSON routes (`/v1/heartbeat`, `/v1/desired`) are frozen and gain
nothing.

## 10. Fit with the two-programs design and the hook contract

A companion design, the two-programs design (`docs/design/two-programs.md`), is proposed in its own pull request in
this repository and is not on `main` yet. It makes hostkeeper two programs, the daemon `hostkeeper` and the service
`coordinator`. Every client is generated from the proto: a bash client over the JSON gateway, and the TypeScript client
the UI uses. There are no hand-written CLI verbs. This design relies on that design for one thing only, the generated
bash client that CI calls, and only P8 waits for it. Until it exists, CI keeps today's `set-desired` path: releases are
not registered, and every host is ordered as today. Nothing else here depends on it.

This design adds no verb to either program:

- Releases are registered by a proto method, called from CI by the generated bash client, so `ci set-desired` is not
  needed for it. The two-programs design keeps today's token properties: a fresh OIDC token per call, kept in a 0600
  file and never in argv or another step's environment. `Register` is one more call made the same way.
- `channel-stage` and `channel-sync` are `xtask` verbs of this repository's release tooling. `channel-stage` is
  deleted at stage 5. `channel-sync` stays only to keep hostkeeper's own legacy builds in the channel (section 9.2).
- Facts are shown by `Hosts.Get` through the generated clients. The UI shows them on the host page, with each host's
  build, or the reason it has none, on the rollout page.
- The daemon serves nothing new on the host. Facts go out in the heartbeat. The stores file is read, never served.

The hook contract (`crates/hostkeeper-hook`, hostkeeper-design.md section 5.3, host-updates.md section 3.2) does not
change. The steps, the command line, the exit codes, the last-line report and the params file stay as they are, and no
new reserved `updater.*` key is added. A hook sees a version directory as before. For a service with an image, it finds
the image under `payloads/` and the reference in its own units, and does its hand-over as it does for processes. A kind
runs no step of the hook's. `load` runs before `prepare`, so `prepare` always finds the version ready to run.

## 11. Tests

Each item's tests are behaviour tests in Rust, in the crate the item changes (rules T8, T11, T12). Each runs with no
network and no system path, through the fakes the crates already have (`Fetcher`, `Verifier`, the coordinator's
backend fake, the probes' command and file ports). None asserts a time: a bound is tested on an injected clock (rule
T10). The failure and boundary cases, by area, with the file each lives in:

- **Facts and probes (P1, P4; `crates/hostkeeper-facts/tests/vocabulary.rs`, `crates/hostkeeper-daemon/tests/facts.rs`).**
  - Each fixture of the shared vectors gives its facts exactly. That covers x86-64 levels v1 to v4 at their edges, a
    missing flag one level below, mixed CPUs (the intersection), arm64 hwcaps up to 8.4, and `aarch64` read as
    `arm64`. A Mac's `sysctl` with `hw.optional.arm64: 0` is refused.
  - A helper that is missing, times out, or prints more than the cap leaves its keys absent and raises the alert.
    `/etc/os-release` without `VERSION_ID` gives no `distro_version`.
  - A version of five components is not a version. `24.04` equals `24.4`.
  - `runtime` lists `podman` when `podman version` answers, and is absent when it is missing or fails.
  - The limits (64 keys, 256 items, 32-byte items, 64-byte keys, 128-byte text, 8 KiB in all) are tested one under and
    one over. Over the total, the largest key is dropped with the alert and the heartbeat still goes out.
- **Matching (P2; `crates/hostkeeper-facts/tests/matching.rs`).** A table of builds by facts:
  - no match, one match;
  - the most specific of a chain (v1 < v3 < v4; macOS 14 < 15);
  - two incomparable matches giving `ambiguous_build`;
  - builds of equal regions refused, however written (`eq linux` against `in linux`; `series 15` against `ge 15`
    with `lt 16`);
  - two constraints on one key intersecting;
  - a key the host lacks, failing;
  - every op of every kind at equal, just-below and just-above values;
  - `series 24.04` against `24.4.1`, `24.10` and `24.40`;
  - two ISA families never comparing;
  - an announced-kind filter removing a match;
  - a daemon without `artifact-v1` matched to its legacy build only.
  
  The cases of section 3.5 are rows of the table. The relation is also checked against brute force: every host value
  of a small domain is enumerated, and "inside" must equal "every host that meets A meets B".
- **Coordinator wire (P3, P6; `crates/hostkeeper-coordinator/tests/facts.rs`, `tests/artifact.rs`).**
  - A heartbeat without facts gives the facts of section 3.4. One whose facts disagree with `platform` is refused.
    `PLATFORM_UNSPECIFIED` with facts is accepted, and without them refused.
  - `artifact` is sent only to a daemon that announced `artifact-v1`, and the feature is echoed to it.
  - A rollback order names the recorded build even when the host's facts changed.
  - A rollback of `hostkeeper` below the first P7 release, on a host holding a non-legacy build, is held
    `rollback_below_artifact`.
- **Register and desired state (P5; `crates/hostkeeper-coordinator/tests/register.rs`).**
  - Every row of section 6.2, one refusal each.
  - An identical re-register answers `created = false`; a different one is `RELEASE_EXISTS`.
  - A legacy index that disagrees with the legacy builds by one digest, one size or one platform is refused.
  - 64 builds is accepted and 65 refused. A body over 64 KiB is refused on both routes.
  - `SetDesired` with the legacy index of a registered version orders its builds. With another index it is refused.
  - The previous coordinator's reader starts on the new files and schema.
- **Daemon fetch (P7; `crates/hostkeeper-daemon/tests/stores.rs`, `crates/hostkeeper-package/tests/fetch.rs`).** Every
  row of section 7.5:
  - a mirror wins;
  - a mirror with wrong bytes, then a location with the right ones (alert, success);
  - only a mirror with wrong bytes (`Deferred`, `store_digest_mismatch`);
  - a mirror that hangs, then a location that answers (per-source budget);
  - every source down (`Deferred`);
  - every location wrong (`Failed`);
  - no store defined;
  - a body one byte over `bytes`;
  - a redirect to `http://`, and one past the store's count;
  - a credential not sent after a redirect to another origin;
  - a token realm on another origin;
  - a bundle over 1 MiB;
  - `stores.toml` with a world-readable credential file, with both credential keys, or with an `oci` mirror without
    `repository`, each refusing the start;
  - no `stores.toml`, which gives the `channel` store.

  On the order: constraints the host no longer meets (`build_unsuitable`); a manifest whose `needs` fail
  (`needs_mismatch`); an `artifact` whose digest is not `package_digest`, or a locator with `..`, `%`, a scheme, `//`,
  a query or 1025 bytes (`NOT_READ`); an order without `artifact` takes the channel path. On the previous release's
  binary: its record reader on records this one writes, and its start on a host holding a variant build.
- **Kinds (P11; `crates/hostkeeper-package/tests/kinds.rs`, `crates/hostkeeper-privileged/tests/oci_image.rs`).**
  - An image manifest whose digest differs from the attested `manifest.toml` is `payload_mismatch`.
  - A layer with one byte changed is refused before it is written. 129 blobs is refused.
  - A `load` that fails in an order is `Deferred`; at daemon start it raises the alert and the start goes on.
  - A sealed layout changed after publish halts the next step (`artifact_changed`).
  - `unload` on the prune leaves an image another kept version uses. An `unload` that fails leaves the version for the
    next prune and raises `unload_failed`.
  - A build with an image and no `runtime has podman` is refused at registration.

This list is complete in this sense: it holds one case for every refusal, outcome, alert and bound named in sections 3
to 7 and 9.1, plus the edges of every comparison. Each item's pull request names its tests (rule T8).

## 12. Build items, one pull request each

| # | Item | After | Proof |
|---|---|---|---|
| P1 | The vocabulary: a jobscheduler pull request proposing the keys and the version rule of section 3.6 item 2; then the key table and test vectors copied here with their SHA-256, and the test that compares them | none | `vocabulary.rs`, failing on a changed copy and on a changed hash |
| P2 | The pure crate `hostkeeper-facts`: kinds, the version grammar, constraints, regions, more specific, the choice of section 4.2 | P1 | `matching.rs`, at 99% lines and branches (rule T1) |
| P3 | Proto `HeartbeatRequest.facts` and `Host.facts`; the coordinator reads facts, maps the legacy platform, keeps facts in the fleet view and shows them | P2 | `additions_facts.rs`; the coordinator's `facts.rs`; a `Hosts.Get` golden through the gateway |
| P4 | The daemon's probes and its heartbeat facts | P3 | The daemon's `facts.rs`; `first_boot.rs` asserting the facts `Hosts.Get` shows (`privileged`, `macos-device`) |
| P5 | Proto `Releases.Register` and `GetRelease`; the releases and host-build records, file and fake; the 64 KiB limit; `SetDesired` and `Force` over registered releases; matching in `decide`, `force` and `converge`; `no_build` and `ambiguous_build`; legacy-only builds to daemons without `artifact-v1` | P2, P3 | `additions_register.rs`; `register.rs`; decide tests over a mixed fleet (old and new daemons, three platforms, variant builds); the previous coordinator starting on the new state |
| P6 | Proto `Artifact` on orders; the coordinator fills it for a daemon that announced `artifact-v1`, and supports (echoes) `artifact-v1`, `kind-archive-v1` and `kind-oci-image-v1`; rollback to the recorded build; the hold `rollback_below_artifact` | P5 | `additions_artifact.rs`; the coordinator's `artifact.rs` |
| P7 | The daemon: `stores.toml`, the `https` and `oci` stores, the fetch of section 7.5, both looks of section 4.3, manifest `platform` against the build and its `needs`, announcing `artifact-v1` and `kind-archive-v1`; the legacy fetch kept | P6 | `stores.rs` and `fetch.rs`; the self-update device tests of section 9.3 through a registered release; the two mixed cases of section 9.3 |
| P8 | This repository's `artifacts.yml` and `release.yml` register each release with its three legacy builds and its legacy index, through the generated bash client, and set it desired by version with the legacy index | P5, the two-programs design's bash client | The workflow lint (`cargo test -p xtask --test workflows`) for the new steps and their token handling; the next release on `main` registered (`Releases.GetRelease` answers it) |
| P9 | Variant builds of `examples/hello` (an x86-64-v3 build beside v1, a macOS 15 build beside 14), with `needs` in their manifests, registered by its workflow | P7, P8 | `examples/hello/tests/package_goldens.rs` for the manifests; `hello` on the farm: each host's build in `Releases.Status` is the most specific it meets |
| P10 | Stage 5: the channel keeps only hostkeeper's legacy builds; `channel-stage` and the other services in `channel-sync`'s trust list go; the coordinator's `--release-channel` check goes; their docs (`docs/deploy/release-channel.md`, `docs/deploy/channel-sync/`) | both conditions of stage 5 | The coordinator's tests that sets with an index are still accepted; the supersession check of rule B22; in the pull request's text, `Hosts.List` showing `artifact-v1` on every host and `Releases.GetRelease` answering each retired service's last three releases |
| P11 | The `oci-image` payload kind: `[[payloads]]`, `closure`, the OCI layout, podman `load` and `unload`, the observer for runtime units, `kind-oci-image-v1` announced | P7 | `kinds.rs`; `oci_image.rs`, a device test in `privileged` installing, updating and rolling back a two-slot image service with podman on the hosted runner |
| P12 | Stage 6: the daemon drops the legacy fetch and its `Platform` copy, and requires `artifact-v1` echoed | P10 | The daemon's `coordinator_too_old` test in `stores.rs`; the device tests of section 9.3 |

P3 and P4 can run beside P5 and P6. P9 and P11 can run beside each other. The docs a behaviour change makes stale move
with that item (rule T19): `docs/package-format.md` with P7 and P11, `docs/hello.md` with P9, `docs/deploy/ci-client.md`
with P8, and hostkeeper-design.md's section 5.5 and the platform line of its section 4.7 with P7. That file is already 1001 lines, so each edit adds no line to
it (rule R12).

## 13. Decisions

| Question | Decision | Why |
|---|---|---|
| Who chooses a host's build | The coordinator; the daemon checks the order's constraints before the fetch and the attested manifest's `needs` after it | Only the coordinator sees every build and the fleet. The second check reads the signer's bytes, so a wrong coordinator cannot put a build on a host its package says it does not fit |
| Rollback and facts | A rollback goes to the recorded build, never matched again | Its target is on the host already, and a host whose facts changed must still be able to go back |
| How "more specific" is computed | Containment of each key's allowed region | Exact and cheap for intervals, token sets and ladders; equal regions written two ways are seen as equal |
| Facts over the size limit | The daemon drops the largest keys and alerts; the coordinator refuses only facts a correct daemon never sends | A host must never drop out of the fleet view because its CPU lists many flags |
| Leading zeros in a version | Allowed, compared as numbers | Distributions write `24.04`; proposed to jobscheduler in P1 |
| Fetch budget with several sources | `fetch_timeout_secs` per source | One hung mirror must not block every update on its hosts |
| A mirror with wrong bytes and no location answering | `Deferred`, environment | A stale mirror is the host's fault, not the release's, and must not count against the release |
| The coordinator's half of `artifact` | Its own item, P6, released with stage 1 | Every coordinator then echoes `artifact-v1` before any daemon announces it, which stage 6 relies on |
| `SetDesired` while old coordinators may come back | CI sends the legacy index with the version until stage 5 | A coordinator rolled back to one before P5 then reads a set it knows, instead of `no_digest` everywhere |
| The channel after stage 5 | Kept for hostkeeper's own legacy builds | A host installed from an old bootstrap package must still update itself (rule B8) |
| Facts: every heartbeat or on change | Every heartbeat, the full set | At most 8 KiB (about 2 KiB on an AVX-512 host) against the 16 KiB heartbeat limit, and a restarted coordinator knows every host after one beat. jobscheduler's revision handshake is not needed with soft state this small |
| Facts the operator sets | None | A fact is what the host is. A different build for a host is a change to the build's constraints, not to the host's facts |
| Constraint keys | A closed list in the coordinator, open values | A key the matcher cannot evaluate would have to be skipped, which is a guess. Values are where the openness is needed |
| Ties | `ambiguous_build`, never a pick | "Most specific" must mean one thing. A pick by order would install builds no one chose |
| Sharing jobscheduler's code | The vocabulary as data with a hash, not a crate | No git sources are allowed and the crates are not published. The vocabulary is what must agree; the code is small |
| hostkeeper's own platforms | Stay three; the `compile_error!` stays | This design opens the services' builds. A fourth platform for the daemon is its own decision |
| The OCI kind's shape | A payload of an archive | The lifecycle (hook, units, health) already exists in the archive. Trust in the image is transitive through the attested manifest, which also covers images this project did not build |
| Runtime | podman, rootful, under systemd units | Daemonless, systemd-native, on the hosted runners for a device test. No runtime on macOS in v1 |
| Where locations live | Per build at registration, sent in the order, resolved against the host's `stores.toml` | Credentials stay on the host, a LAN mirror needs no registration, and an order carries no absolute URL |
| Stores file | `/etc/hostkeeper/stores.toml`, apart from `hostkeeper.toml` | The previous binary must still start on `hostkeeper.toml` after a fallback |
| Default with no stores file | One store, `channel`, at `release_channel` | A new daemon on an old host keeps fetching where it did |
| Release immutability | A release is registered once; an identical repeat is a no-op | CI retries are safe, and a version names one set of bytes, as the channel's immutability rule does today |
| The coordinator verifying attestations at registration | No | The daemon is the one verifier, with the pinned `gh` and trusted root. A second verifier in the coordinator's image would be a second policy to keep equal |
| Where releases are persisted | A separate file or table | `deny_unknown_fields` on the registry and the state file would refuse a new key after a rollback |
| Upload by CI to a mirror | None | The GitHub release is already published and attested. A mirror pulls, so no new write path exists (rule S3) |
| `GATE_REASON_NO_PACKAGE_FOR_PLATFORM` | Kept with its meaning; `NO_BUILD` added | Widening a shipped value's meaning is not additive (rule B8) |
| Payload count and size | 128 blobs, `max_package_bytes` in total | The archive's bound, unchanged, so a payload cannot exceed what one package may |

## 14. Questions for the maintainers

1. **The OCI kind in v1.** The design builds it (P11) because the coordinator's own stack runs in containers. If no
   service needs it yet, P11 waits and v1 is the archive alone. The contract of section 5.1 stays, so it can land later.
2. **The keys new to jobscheduler** (section 3.6 item 2). They are proposed there first. If jobscheduler declines one,
   hostkeeper keeps it under a `hostkeeper.` prefix, which jobscheduler's namespacing rule allows.
3. **The hostkeeper-only channel after stage 5.** The design keeps it, so a host installed from an old deb or pkg can
   still update itself (rule B8). Retiring it too would raise the bootstrap floor: such a host would need the current
   deb, rpm or pkg before it could update. That needs the maintainers' waiver of B8, so the design does not plan it.
4. **A registry whose token service is on another origin** (section 7.4). It is out of v1. Allowing one would need a
   `token_origin` key per store in `stores.toml`. tierstore needs none.
