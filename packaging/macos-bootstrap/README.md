# The hostkeeper bootstrap pkg

`hostkeeper-bootstrap-<version>-macos-arm64.pkg` is the one-time pkg that Apple Business (a
Blueprint) installs on a new Mac (docs/design/mac-bootstrap.md). It carries no secret and no farm
value. `cargo xtask release macos-bootstrap-pkg` builds it from the `hostkeeper-bootstrap`
binary and the two files here, and `check-macos-bootstrap-pkg` reads the built pkg back.

| Installed at | Mode | Source |
|---|---|---|
| `/usr/local/lib/hostkeeper-bootstrap/hostkeeper-bootstrap` | 0755 | the binary of `crates/hostkeeper-bootstrap` |
| `/Library/LaunchDaemons/dev.hostkeeper.bootstrap.plist` | 0644 | `dev.hostkeeper.bootstrap.plist` |

The pkg's one script is `postinstall` (this directory, mode 0755): it loads the launchd job
`dev.hostkeeper.bootstrap` when launchd has not loaded it. The job runs the bootstrap at load and
again only after a run that failed (`KeepAlive` `SuccessfulExit` false, one start a minute at
most); its output is `/var/log/hostkeeper-bootstrap.log`. The component's identifier is
`dev.hostkeeper.bootstrap`.

The bootstrap reads the coordinator's address and the CA's SHA-256 from the managed preferences of
the domain `dev.hostkeeper.config` (a Blueprint configuration), or from
`/Library/Application Support/hostkeeper/bootstrap.toml` on a Mac set up by hand.
