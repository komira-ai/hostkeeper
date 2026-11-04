# Package format

A hostkeeper package is one archive per version of a service, `<service>-<version>.tar.zst`.
CI computes its SHA-256 and puts it in the desired state with the version; the daemon fetches the
archive, checks that digest and an attestation for it, and only then reads it. This page is what
the daemon then requires of the archive and what it leaves on the host. Every part of it is a
format that another release of hostkeeper reads, so it changes only as the last section says.

The code is `crates/hostkeeper-unpack` (the archive reader, the rules, the publish) and
`crates/hostkeeper-steps` (the tree digest, the seal and the layout, which the step runner checks
before it runs anything from a version).

## The archive

One zstd frame holding one tar archive.

### Compression

- Exactly one zstd frame, and nothing after it: no second frame, no skippable frame, no other
  byte.
- The frame's window is at most 8 MiB (8388608 bytes), the window of `zstd -19` and of every
  lower level. Build packages with `zstd -19` or lower and without `--long`; a frame that
  declares a larger window is refused before the decoder allocates anything for it.
- Dictionaries are not used. A content checksum (written by `zstd` by default) is checked when
  it is present, and a declared content size must be what the frame decompresses to. A
  single-segment frame's window is its content size, so it too is at most 8 MiB. The reserved
  bit of the frame header must be clear.

### The tar archive

ustar headers, POSIX (`ustar\0` `00`) or GNU (`ustar  \0`), each with a correct checksum, read
in one pass from the front. The extensions read are the pax extended header (type `x`), with
only the records `path`, `size`, `mtime`, `uid`, `gid`, `uname` and `gname` (the last five are
ignored), and the GNU long name (type `L`). An extended header holds at most 8 KiB of records
and is followed by the entry it describes.

Each entry is checked against every rule below before any byte of it is written. The rules, in
the order of the design's bounds table (docs/design/hostkeeper-design.md, section 5.5):

**Bounds.**

| Bound | Limit |
|---|---|
| Decompressed bytes, headers and padding included, counted as they leave the decoder | `max_unpacked_bytes` of the daemon config |
| Entries (files and directories) | `max_entries` of the daemon config |
| A name (the full path) | 255 bytes |
| Depth of a name | 16 components |
| `manifest.toml` | 64 KiB (65536 bytes) |

**Shape.**

- At the top: the regular file `manifest.toml` and the directories `bin`, `units` and `files`,
  nothing else. Below the top, only inside those three directories.
- Regular files and directories only. Hard links, symbolic links, character and block devices,
  fifos, pax global headers and every other tar type are refused.
- A directory's entry comes before the entries inside it.
- A name is UTF-8, relative, with no empty, `.` or `..` component and no NUL. A directory's
  name may end with one `/`.
- No name twice, and no two names equal after simple lower-case folding of the whole path
  (`bin/X` beside `bin/x`).
- No extended attributes or ACLs (pax records `SCHILY.xattr.*`, `LIBARCHIVE.xattr.*`,
  `SCHILY.acl.*`, `security.*`), and no other pax record than the seven above.
- No setuid, setgid or sticky bit in an entry's mode.
- After the end-of-archive marker (two zero blocks), nothing but zero bytes, a whole number of
  512-byte blocks in all, then the end of the frame. A truncated archive is refused.

A package that breaks a rule is refused as a bad package (`Failed`, class `release`,
`bad_archive`), and the staging directory is removed.

## Unpacking

The archive is unpacked into `packages/<service>/.stage-<random>/`, created with mode 0700, from
a handle on the packages directory that never follows a link, so no entry can write outside it.

- **Owner and times.** Every file and directory is owned by root. The archive's owner, group,
  user and group names and times are not used.
- **Modes.** A file whose tar mode has any execute bit gets 0555, every other file 0444, every
  directory 0555 (set after its contents are written).

Then `manifest.toml` is read by the manifest loader (`name` and `version` must be the order's;
the load rules are those of the design's section 4.7), the tree is made durable and read back,
and it must be what was written.

## The tree digest

The digest of a version directory: the lower-case hex SHA-256 of one record per entry below the
directory (the directory itself is not an entry), in the byte order of the entries' full names.

```text
record  = length header [bytes]
length  = u64, little-endian: the length of header plus, for a file, its size
header  = kind NUL name NUL mode NUL size NUL
kind    = "f" (a regular file) or "d" (a directory)
name    = the full relative name, as bytes, "/" between components
mode    = the permission bits (mode & 0o7777), four octal digits: "0555" or "0444"
size    = the size in bytes, in decimal; "0" for a directory
bytes   = the file's contents (files only)
```

The owner and the times are not hashed: the archive does not carry them into the tree. A tree
holding anything other than regular files and directories has no digest. The test
`the_tree_digest_has_a_fixed_value` holds the digest of a fixed tree as a literal.

## The seal

`packages/<service>/<version>.seal`, root-owned, mode 0444:

```json
{"schema":1,"tree_digest":"<64 hex digits>","package_digest":"<64 hex digits>"}
```

`tree_digest` is the tree digest of `<version>/`; `package_digest` is the SHA-256 of the archive
it was unpacked from. Before the daemon uses a version it recomputes the tree digest and compares
it with the seal; a version that changed is not run. A reader ignores fields it does not know.

## On disk

```text
/var/lib/hostkeeper/packages/<service>/
  <version>/            the unpacked tree: manifest.toml, bin/, units/, files/
  <version>.seal        its seal
  current  -> <version> the version the host runs
  previous -> <version> the version it ran before the last switch of current
  .stage-<random>/      an unpack under way
  .<version>.seal.tmp   a seal being written
```

The links' targets are relative (`1.5.0`, not a path). No version starts with `.`, so the names
of an unpack under way cannot be a version, and a version is never `current`, `previous` or a
name ending in `.seal` or `.tmp` (refused as `reserved_version`). The service directory is
0755, so the service's users reach their version; a `.stage-*` directory is 0700 until it
becomes the version.

One unpack of a service runs at a time: an unpack holds an exclusive lock (`flock`) on the
service directory from before it removes orphans until it has published, and `remove_orphans`
takes the same lock, so neither removes the other's work. A second holder is refused at once, to
try again later.

The publish keeps a version directory whole or absent:

1. Write `.<version>.seal.tmp`, fsync it.
2. Rename it to `<version>.seal`; fsync the service directory.
3. Rename `.stage-<random>` to `<version>`; fsync the service directory.

A version directory therefore never exists without its seal. What a crash leaves (a
`.stage-*` directory, a `.<version>.seal.tmp`, a seal with no version beside it) is an orphan:
every unpack removes the orphans of its service before it stages, and `remove_orphans` is the
same removal for the daemon to run at start and before each fetch of that service. A version that is already on the host,
sealed for the same archive and unchanged, is used as it is.

## Changing the format

Every part of this page changes additively only (AGENTS.md B8):

- A new field of the seal is optional; readers ignore fields they do not know.
- A change that a reader of today must not take for what it knows (another digest, another
  meaning of a field) is a new `schema`. A reader refuses a schema it does not know rather than
  misread it.
- The tree digest and the on-disk names do not change in place: a new digest is a new field of
  a new seal schema.
- A new archive rule may refuse packages that were accepted before only when no released
  package breaks it; a rule is never relaxed without a design change.

The tests `the_seal_and_tree_digest_formats_are_additive_only` (the committed digest of a fixed
tree, a seal with a field this release does not know), `the_seal_is_read_additively` and
`the_on_disk_names_are_current_previous_and_version_seal` hold these formats.
