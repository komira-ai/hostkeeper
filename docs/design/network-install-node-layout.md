# Network install: extra partitions and kernel parameters of a node

The seed of a network install (network-install.md, section 4, "The seed";
`crates/hostkeeper-pxe-seed/src/seed.rs`) can lay out partitions beside root on the install disk and add words to the
installed kernel's command line, for a node that needs either at install (komira-ai/hostkeeper#496). Without either input
the seed is the bytes it was before them.

## Why

tierstore's fetch, cache and serve design (komira-ai/tierstore, `docs/design/fetch-cache-serve.md`) needs both:

- A build node's tierstore cache is its own xfs partition of the node's SSD, mounted `noatime`, not a loop image on
  root (its section 11: 220 GiB for a 185 GiB budget); a combined node also has a partition for the hot tier (its
  section 21.1). Root is ext4 filling the SSD, so a live repartition is not an option, and the partitions are made at
  install.
- `fuse.enable_uring` is set at boot only (its section 5.2, item 4); fuse is built into the nodes' kernel, so the
  kernel command line is the way.

## Extra partitions

`pxe serve --extra-partition <GPT label>:<size>:<file system>:<mount point>[:<mount options>]`, repeatable, in the
order of the disk; `extra-partition`, an array, in the settings file. The label is 1 to 36 of `A-Za-z0-9._-`, starting
with a letter or a digit; the size `<n>MiB` or `<n>GiB`; the file system `xfs` or `ext4`; the mount point an absolute
path that is not `/`, nor `/boot` or below it; the options a comma-separated list of `A-Za-z0-9._=-` words. A label or a
mount point given twice, or more than 16 partitions, refuses the start (`crates/hostkeeper-pxe-seed/src/partitions.rs`).

With one, the seed's storage is a storage config in place of the layout `direct`, on the disk `--install-disk` matches,
matched the same way:

1. The disk, wiped, with a GPT table.
2. Partition 1, the EFI system partition: 1 GiB, fat32, the one GRUB is installed to, at `/boot/efi`.
3. Each extra partition in the order given, with its GPT label, size in bytes and file system.
4. Root, ext4 at `/`, last, with size `-1`: the rest of the disk.
5. The mounts: root, the EFI system partition, then the extra ones in their mount points' order, so one below another
   is mounted after it; each with its options when given.

That the 24.04 installer makes this config as written is **[V]** at the first install of a node with extra partitions.
A disk too small for the partitions and a root fails the install, at the installer: `pxe` cannot know a disk's size.

## Extra kernel parameters

`pxe serve --kernel-parameter <name>[=<value>]`, repeatable; `kernel-parameter`, an array, in the settings file. A word
is 1 to 256 printable ASCII characters with no space, `"`, `\`, `$` or backtick, since it is written inside a
double-quoted shell string, and a name of `A-Za-z0-9._-`. A name given twice, with or without a value, or more than 32
words, refuses the start (`crates/hostkeeper-pxe-seed/src/kernel.rs`).

The words go in their own GRUB defaults file, `/etc/default/grub.d/98_hostkeeper_parameters.cfg`, one line that adds
them to `GRUB_CMDLINE_LINUX` in their order. They are not added to the serial console's file,
`/etc/default/grub.d/99_hostkeeper.cfg`, as the issue suggested: the daemon keeps that file at the console's bytes
alone (`crates/hostkeeper-daemon/src/serial_baseline.rs`), and would take them out. The first boot's cloud-init writes
the file, and one `update-grub` applies it and the console's, so the parameters are in force from the second boot, as
the console's `console=` is.

## One run, one layout

Both are flags of `pxe serve`, like `--install-disk` and `--serial-console`, so every machine one run installs gets the
same partitions and parameters. A farm whose machines need different layouts installs them in separate runs. The
inventory record carries neither: that would be a change to the coordinator's record and its API for an input only the
seed reads.

## Tests

`crates/hostkeeper-pxe-seed/tests/seed/extras.rs`: goldens of the seed with the partitions of a combined node, with
`fuse.enable_uring=1`, and with both and the serial console, each also read back as YAML; mounts in mount-point order;
each `--install-disk` form as the config's disk; empty lists give the earlier goldens. `tests/partitions.rs` and
`tests/kernel.rs`: each grammar at its bounds and every refusal by its message. In `crates/hostkeeper-pxe`: the
settings file's arrays reach the flags, a value given twice refuses the start, and the flags reach the seed served.
