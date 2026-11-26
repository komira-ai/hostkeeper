//! The methods of a generated service that are written by hand: `hk xcode verify` is a method of
//! the service `xcode`, which the table of the protos gives only `list` and `remove`. A hand
//! method is found by the service's command word and its own, listed after the generated
//! methods in `hk <service> --help` and in `docs/cli.md`, and run by its function in place of a
//! gateway call. A service that the table does not have has none.

use crate::args::Globals;
use crate::{Flow, World};

/// What a hand method runs: the words after its name, and the global flags given before them.
type Run = fn(&mut World<'_>, Globals, &[Vec<u8>]) -> Flow<()>;

/// A flag of a hand method, for `docs/cli.md`.
pub(crate) struct Flag {
    pub(crate) name: &'static str,
    pub(crate) value: &'static str,
    pub(crate) help: &'static str,
}

/// A hand-written method of a generated service.
pub(crate) struct Method {
    /// The command word of the service, as the table has it (`xcode`).
    pub(crate) service: &'static str,
    /// The command word of the method.
    pub(crate) method: &'static str,
    /// What it does, in the one paragraph `--help` and `docs/cli.md` show.
    pub(crate) doc: &'static str,
    /// Its own flags, none or more (the global flags are not listed here).
    pub(crate) flags: &'static [Flag],
    pub(crate) run: Run,
}

/// Every hand method.
pub(crate) const METHODS: &[Method] = &[
    Method {
        service: "xcode",
        method: "add",
        doc: "Brings an Xcode release into the coordinator's store, from the operator's own Mac or, \
              with --mac, from a system without macOS's programs: `hk xcode add` takes the newest \
              Xcode_<version>.xip in ~/Downloads (or the release waiting, downloaded with xcodes), \
              `hk xcode add 27.2` the archive of that version (downloaded with xcodes when \
              ~/Downloads has none), `hk xcode add <path>` that file. On macOS it checks the \
              archive's kind and Apple's signature, reads its version and build from the expanded \
              app (keeping a record that saves the next expansion), asks the store first (an \
              archive it holds ends the run with exit 0), uploads with progress, resuming where \
              the store holds a part of the archive (a run that stops says how far it got, and \
              running it again continues), and ends with the Macs that are due the release and \
              when, as the coordinator's Schedule preview answers it. Off macOS, `--mac <node-id>` \
              (or xcode_mac in ~/.hostkeeper/hk.toml) names a farm Mac: the archive is not \
              downloaded or expanded here, it is uploaded as above (its version and build are the \
              file's name and the coordinator's waiting release), and then a maintenance run of \
              the kind probe has that Mac check it from the store (Apple's signature, the \
              expansion, the version and build, the app's code signature) while `hk` waits and \
              says each step; a pass writes the record, and an archive that fails is removed from \
              the store. One run at a time per system. Not a gateway call. `hk xcode add --help` \
              says each step; `hk xcode-push` is a second name for one release.",
        flags: &[Flag {
            name: "--mac",
            value: "NODE-ID",
            help: "Off macOS: the node id of the farm Mac that checks the archive, in place of \
               xcode_mac in ~/.hostkeeper/hk.toml; it must be a Mac whose hostkeeper lists probe \
               on demand, in a host group that keeps Xcode (xcode_keep 1 or more). A usage error \
               on macOS, where the checks run on this Mac.",
        }],
        run: crate::xcode_add::xcode_add,
    },
    Method {
        service: "xcode",
        method: "verify",
        doc: "Checks an Xcode archive on the operator's own Mac, or, with --mac, on a farm Mac, and \
          writes the record of what it found to ~/.hostkeeper/xcode/<sha256>.json, mode 0600, and \
          to stdout. On macOS: the programs, the archive's kind, Apple's signature and published \
          root, a damaged copy and a file that is no archive, the archive's table of contents, \
          and the expansion with the version and build read from the app; no coordinator is \
          asked. Off macOS, `--mac <node-id>` (or xcode_mac in ~/.hostkeeper/hk.toml) names the \
          farm Mac, and the archive must be in the coordinator's store: a maintenance run of the \
          kind probe has that Mac check it (Apple's signature, the expansion, the version and \
          build, the app's code signature) while `hk` waits, and the record holds the Mac's \
          steps. Not a gateway call. Exits 0 when the verdict is pass and 1 otherwise; `hk xcode \
          verify --help` says each check.",
        flags: &[
            Flag {
                name: "--apple-root",
                value: "FILE",
                help: "Apple's root certificate (DER or PEM), in place of fetching it over HTTPS \
                   from Apple's certificate authority page. A usage error off macOS.",
            },
            Flag {
                name: "--mac",
                value: "NODE-ID",
                help: "Off macOS: the node id of the farm Mac that checks the archive, in place \
                   of xcode_mac in ~/.hostkeeper/hk.toml; it must be a Mac whose hostkeeper lists \
                   probe on demand, in a host group that keeps Xcode (xcode_keep 1 or more). A \
                   usage error on macOS, where the checks run on this Mac.",
            },
        ],
        run: crate::xcode_verify::xcode_verify,
    },
];

/// The hand methods of the service whose command word is `service`.
pub(crate) fn of(service: &str) -> impl Iterator<Item = &'static Method> {
    METHODS.iter().filter(move |m| m.service == service)
}

/// The hand method `method` of `service`.
pub(crate) fn find(service: &[u8], method: &[u8]) -> Option<&'static Method> {
    METHODS
        .iter()
        .find(|m| m.service.as_bytes() == service && m.method.as_bytes() == method)
}
