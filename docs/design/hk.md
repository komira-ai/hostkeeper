# hk: one command line

Status: built. The code is `crates/hostkeeper-hk` (library `hostkeeper_hk`, one binary `hk`). It replaces
`crates/hostkeeper-hkctl` (the bash `hkctl`) and `crates/hostkeeper-hkssh` (the Rust `hkssh`), both deleted.
It supersedes, where they disagree, the statements of [two-programs.md](two-programs.md) (section 2.4 and the
decisions about the CLI's transport and secrets) and of [ssh-through-coordinator.md](ssh-through-coordinator.md)
(decision 5) that the command line is a bash script needing `curl`, and that a separate `hkssh` carries the ssh
tunnel. Each of the two has an Update note at its top.

## 1. What changes for the operator

**Before.** The operator's command line was `hkctl`, one bash script (bash 3.2 or later, `curl` 7.55 or later)
downloaded as the release asset `hkctl-<version>`. Its ssh commands ran a second program, `hkssh`, a Linux
release asset of its own that had to be unpacked beside it. Two downloads, two version strings, two
verifications.

**After.** The command line is `hk`, one Rust program, with the ssh commands built in:

- `hk <service> <method> [flags]` is every method of the operator API, as `hkctl` was.
- `hk ssh <host-id>`, `hk ssh-proxy <host-id>`, `hk ssh-revoke` and `hk ssh-status <host-id>` are the ssh
  commands. There is no `hkssh`.
- It is downloaded as `hk-<version>-<platform>.tar.gz` for `linux-x86_64`, `linux-arm64` and `macos-arm64`,
  verified (the release's `SHA256SUMS`, then `gh attestation verify`, as
  [verify-a-release.md](../deploy/verify-a-release.md) says) and unpacked. It needs no bash, no `curl` and no `jq`
  installed to make a call.
- `hkctl` is the old name. The tarball holds a link `hkctl` to `hk`, so a script that calls `hkctl` keeps working.

An operator's `ProxyCommand` line in `~/.ssh/config` is
`ProxyCommand hk --endpoint URL --ca-file PATH --token-file PATH ssh-proxy %h`.

**What does not change.** The services and methods, every flag and its meaning, the global flags (`--endpoint`,
`--ca-file`, `--token-file`, `--token-stdin`, `--github-oidc-audience`, `--response-file`, `--timeout-secs`), the
answer on stdout, the exit codes (0, 1, 2, 3), the `refused: <reason>` token on stderr, the check that a token
file is readable by its owner alone, the `coordinator_older` answer, the files under `$HOME/.hostkeeper/ssh/`
(existing keys, certificates and pins keep working), and the wire: no API, proto or gateway route changes.

## 2. How it is made

`crates/hostkeeper-hk/build.rs` reads the command table `hostkeeper_proto::ctl::COMMANDS`, which the build of
`hostkeeper-proto` generates from the protobuf services, and `hk` is built from it: a new method of a service is
a new command with no hand edit, as before. [docs/cli.md](../cli.md) is generated from the same table by
`hostkeeper_hk::markdown`, and the test `docs_cli_md_is_current` fails when the checked-in file differs. A method
the gateway does not serve (a stream such as `Ssh/Tunnel`) is not a command of the table; the ssh verbs are the
hand-written part, and `hk --help` lists them.

`src/main.rs` only forwards to `hostkeeper_hk::main()`. Everything the program reads from its surroundings
(arguments, the standard streams, `HOME`, the runner's two OIDC variables, the ssh launcher) reaches the library
as a value, so the tests run the whole program in process against a loopback server and compare requests,
stdout, stderr and exit codes exactly.

The version is `hk <X.Y.Z>+<12 hex digits of the release commit>` in a release build and `hk <X.Y.Z>` in a
development build. `cargo xtask release check-binary` compares `hk --version` with the release's version and
commit.

## 3. The transport

A gateway call is made by the program itself, with no child process, no `curl` and no proxy:

- `POST <endpoint><path>` over HTTPS only, with `Authorization: Bearer <token>` and
  `Content-Type: application/json`. The body is the JSON the flags and `--request` file describe.
- The roots trusted for the coordinator are the PEM certificates of `--ca-file` and no others (TLS 1.2 or later).
  Redirects are not followed, no proxy is used, and an HTTP status is never an error by itself: the gateway's body
  and the status decide the exit code, as before.
- `--timeout-secs` bounds the connection; the whole call is bounded by it plus the hold of a method that holds
  the call (`wait_secs`).
- The `GET /v1/version?at_least=<version>` probe that follows an `invalid_request` follows the same rules and
  carries no token.
- In a GitHub Actions job, `--github-oidc-audience` asks the runner for an ID token with the system's roots,
  using the two variables the runner sets. They are read once at start, are never passed to a child, and the token
  is masked with `::add-mask::` before it is used.

A failure to reach the coordinator is exit 1 with `no answer from <endpoint> (<the error>)`, where the bash
program quoted `curl`'s exit status; that is the one message that is not byte for byte what `hkctl` printed.

## 4. The ssh tunnel, built in

The ssh commands are the code of the former `hkssh`, moved into `src/ssh/`, behind the same global flags as a
gateway call (checked in the same order, with the same messages). `hk ssh` signs a one-host certificate and runs
`ssh` with a `ProxyCommand` that is the running binary's own path with `--endpoint`, `--ca-file`, `--token-file`
and `--timeout-secs` and the verb `ssh-proxy`, and a `KnownHostsCommand` that is the same binary's
`ssh-known-host <pins file> %H %t %K %f`; `ssh-known-host` is a verb only ssh runs. The token is a file name in
those lines, never a value. `ssh`, `ssh-keygen` and `ssh-add` come from `PATH`, the agent socket is inherited, and
the tunnel's keepalive is 30 seconds. The default audit reason of a certificate is now `hk ssh` (or
`hk ssh-proxy`) in place of the former program's name. The design of the tunnel itself is
[ssh-through-coordinator.md](ssh-through-coordinator.md) and is unchanged.

## 5. The program name

`hostkeeper_hk::program_name` is `hkctl` when the file name of argv[0] is exactly `hkctl`, and `hk` otherwise.
That name is the prefix of a diagnostic (`<name>: <message>`), the first word of `--version`, and the word in a
message that names the program. A script that calls the `hkctl` link therefore sees what it saw from the bash
program. Help texts and `docs/cli.md` always say `hk`.

## 6. Release assets

Each release has, for `linux-x86_64`, `linux-arm64` (static, musl) and `macos-arm64`:
`hk-<version>-<platform>.tar.gz` holding `hk` and the link `hkctl` to it, its CycloneDX SBOM
`hk-<version>-<platform>.cdx.json` and its attestation bundle `hk-<version>-<platform>.tar.gz.sigstore.json`.

Withdrawn, no longer published from the first release that ships `hk`: `hkctl-<version>`,
`hkctl-<version>.sigstore.json` and the six `hkssh-<version>-<platform>` assets (tarball, SBOM, bundle for each of
two Linux platforms). They are listed as withdrawn in the release contract, so a check does not look for them.
Releases already published keep theirs.

The set-desired workflow of CI downloads `hk-<v>-linux-x86_64.tar.gz` and `SHA256SUMS`, verifies the checksum and
the attestation, unpacks the archive and runs `hk`. Its input `hostkeeper_version` must name a release that ships
`hk`; callers pin the workflow by commit, so a pinned caller keeps the behaviour it was pinned to.

`hk` is not in the deb, the rpm or the macOS installer: no host needs it.

## 7. Compatibility and failure modes

- **Scripts.** A script that calls `hkctl` with any flags keeps working through the link and sees the same
  output and exit codes. A script that downloaded the asset `hkctl-<version>` of an older release is unaffected;
  of a newer release, it must fetch `hk-<version>-<platform>.tar.gz`.
- **A proxy in the environment** is not used, where `curl` would have used it. A coordinator reached only through
  a proxy is not reachable by `hk`.
- **System roots** are not trusted for the coordinator, as before; a call needs `--ca-file`.
- **The token file's mode** is judged as each program judged it: a gateway call refuses a file its group or
  others may read (as `hkctl` did), the ssh verbs a file with any group or other bit (as `hkssh` did).
- **On macOS** the release checks the `hk` of the tarball by its code signature and its `--version`; the macOS
  beta stage runs the tests of the crates `hostkeeper` and `hello` are built from, which `hk` is not.
- **A program called otherwise** than `hk` or `hkctl` (a renamed copy) says `hk`.
- **ssh state** is read from the same directory, so an operator who used `hkssh` keeps the key, the certificates
  and the pins; an old `ProxyCommand hkctl ... ssh-proxy %h` line works through the link.
- **The `ssh` programs** (`ssh`, `ssh-keygen`, `ssh-add`) are needed by the ssh verbs only; gateway calls run no other program.
- **Wire formats** are not touched, so a new `hk` against an old coordinator and the reverse behave as `hkctl` did;
  the `coordinator_older` answer is kept.

## 8. Tests

Behaviour tests of `crates/hostkeeper-hk` run the program in process: the request bytes of every kind of flag,
the answer and exit code of each gateway status, the refusal tokens, the token-file mode refusal, the OIDC
token request, the version probe, the `hkctl` name, the ssh verbs against a fake coordinator and fake programs,
and `docs_cli_md_is_current`. They use loopback sockets on port 0 and scratch directories, and assert no timing.
