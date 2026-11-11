# The tree digest and the seal

The formats of a version directory of a service under `/var/lib/hostkeeper/packages/<service>/`: the tree digest of its contents, the seal that
holds it, and the names around them. A package is a native `.deb` or `.pkg` (see [packages-native.md](design/packages-native.md)); these
formats are what the host keeps of a version that was placed.

## The tree digest

The digest of a version directory: the lower-case hex SHA-256 of one record per entry below the
directory (the directory itself is not an entry), in the byte order of the entries' full names.

```text
record  = length header [bytes]
length  = u64, little-endian: the length of header plus, for a file or a link, its size
header  = kind NUL name NUL mode NUL size NUL
kind    = "f" (a regular file), "d" (a directory) or "l" (a symbolic link)
name    = the full relative name, as bytes, "/" between components
mode    = the permission bits (mode & 0o7777), four octal digits: "0555" or "0444"; "0000" for a link
size    = the size in bytes, in decimal; "0" for a directory; the length of the target text for a link
bytes   = the file's contents (files only); the link's target text (links only)
```

The owner and the times are not hashed: the placed tree does not carry them. A link's
own mode bits are not hashed either (they mean nothing and differ between Linux and macOS): its
record has mode `0000`, and its bytes are the target text as `readlink` gives it, never followed
or resolved. A link is an entry only inside the `tree/` directory of a version directory (the
name has `tree` as its first component and at least one more, as in `tree/usr/lib/x`), where a
repository installer keeps what `dpkg-deb --extract` made; a link anywhere else, `tree` itself
included, and anything other than regular files, directories and those links, give no digest. The
check of a staged release never follows a link and still refuses a foreign owner, a setuid or
setgid bit and a group- or other-writable file or directory; a hook program and a `place` source
must not pass through a link. A tree with no link has the digest it had before links were allowed. The test
`the_tree_digest_has_a_fixed_value` holds the digest of a fixed tree as a literal.

## The seal

`packages/<service>/<version>.seal`, root-owned, mode 0444:

```json
{"schema":1,"tree_digest":"<64 hex digits>","package_digest":"<64 hex digits>"}
```

`tree_digest` is the tree digest of `<version>/`; `package_digest` is the SHA-256 of the package
file the version was placed from. Before the daemon uses a version it recomputes the tree digest and compares
it with the seal; a version that changed is not run. A reader ignores fields it does not know.

## On disk

```text
/var/lib/hostkeeper/packages/<service>/
  <version>/            the placed tree
  <version>.seal        its seal
  current  -> <version> the version the host runs
  previous -> <version> the version it ran before the last switch of current
  .stage-<random>/      a placement under way
  .<version>.seal.tmp   a seal being written
```

The links' targets are relative (`1.5.0`, not a path). No version starts with `.`, so the names
of a placement under way cannot be a version, and a version is never `current`, `previous` or a
name ending in `.seal` or `.tmp` (refused as `reserved_version`). The service directory is
0755, so the service's users reach their version; a `.stage-*` directory is 0700 until it
becomes the version.

One placement of a service runs at a time: a placement holds an exclusive lock (`flock`) on the
service directory from before it removes orphans until it has published, and `remove_orphans`
takes the same lock, so neither removes the other's work. A second holder is refused at once, to
try again later.

The publish keeps a version directory whole or absent:

1. Write `.<version>.seal.tmp`, fsync it.
2. Rename it to `<version>.seal`; fsync the service directory.
3. Rename `.stage-<random>` (still 0700, every directory below it already 0555) to
   `<version>`; fsync the service directory.
4. Make `<version>` 0555; fsync it.

The tree is renamed before its top directory is made read-only because macOS refuses to rename
a directory its caller cannot write, even to a new name in the same parent; the mode of the top
directory is not part of the tree digest, so the seal is the same either way. A version directory
therefore never exists without its seal. What a crash leaves (a
`.stage-*` directory, a `.<version>.seal.tmp`, a seal with no version beside it) is an orphan:
every placement removes the orphans of its service before it stages, and `remove_orphans` is the
same removal for the daemon to run at start and before each fetch of that service. A version that is already on the host,
sealed for the same package and unchanged, is used as it is, its directory made 0555 (finishing
step 4 when a crash came between it and step 3).

## Changing the format

Every part of this page changes additively only (AGENTS.md B8):

- A new field of the seal is optional; readers ignore fields they do not know.
- A change that a reader of today must not take for what it knows (another digest, another
  meaning of a field) is a new `schema`. A reader refuses a schema it does not know rather than
  misread it.
- The tree digest and the on-disk names do not change in place: a new digest is a new field of
  a new seal schema.

The tests `the_seal_and_tree_digest_formats_are_additive_only` (the committed digest of a fixed
tree, a seal with a field this release does not know), `the_seal_is_read_additively` and
`the_on_disk_names_are_current_previous_and_version_seal` hold these formats.
