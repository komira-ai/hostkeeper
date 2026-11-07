# Network boot pins

The files a bare machine boots during a network install (docs/design/network-install.md, sections
4 and 5). For each of `linux-x86_64` and `linux-arm64` there is one boot set: Ubuntu's live-server
ISO, and the shim, the signed network GRUB, the kernel and the initrd of the same release. Each
file is pinned by its size and SHA-256.

| file | what |
|---|---|
| `pins.toml` | the pins: for each platform and each file of its boot set, where it comes from, its size in bytes and its SHA-256 |

`cargo xtask release netboot --pins packaging/netboot/pins.toml --version <version> --dir <dir>`
renders the pins into `<dir>/netboot.json`, a release asset. The artifacts workflow
(`.github/workflows/artifacts.yml`) runs it in its `sums` job, so `SHA256SUMS` lists
`netboot.json`, and its `attest` job attests it with a bundle of its own,
`netboot.json.sigstore.json`. The verb reads no network and no file but the pins. It refuses pins
that miss a platform or a file, that carry an unknown key, a size that is not a whole number above
0, a digest that is not 64 lowercase hex digits, a URL that is not `https://`, a path that is not
relative or holds an empty, `.` or `..` part, or an ISO or deb of the wrong architecture. Each
refusal names the key. Its tests (`crates/xtask/src/release/netboot/tests.rs`) render this file
and hold each refusal to its exact text.

## Where each file comes from

Canonical publishes the two ISOs on different sites. Each site has a `SHA256SUMS` that lists the
ISOs of its directory, and Canonical signs it in `SHA256SUMS.gpg` beside it, with the Ubuntu CD
Image Automatic Signing Key (2012), fingerprint `8439 38DF 228D 22F7 B374 2BC0 D94A A3F0 EFE2
1092`.

| platform | ISO | listed in |
|---|---|---|
| `linux-x86_64` | `https://releases.ubuntu.com/24.04/ubuntu-24.04.5-live-server-amd64.iso` | `https://releases.ubuntu.com/24.04/SHA256SUMS` |
| `linux-arm64` | `https://cdimage.ubuntu.com/ubuntu/releases/24.04/release/ubuntu-24.04.5-live-server-arm64.iso` | `https://cdimage.ubuntu.com/ubuntu/releases/24.04/release/SHA256SUMS` |

The four smaller files of each set are taken from inside its ISO, so the ISO's signed checksum
covers them:

| file | served as | inside the ISO |
|---|---|---|
| shim | `hostkeeper/shimx64.efi`, `hostkeeper/shimaa64.efi` | `usr/lib/shim/shim<efi arch>.efi.signed.latest` of the deb `pool/main/s/shim-signed/shim-signed_<version>_<deb arch>.deb` |
| network GRUB | `hostkeeper/grubx64.efi`, `hostkeeper/grubaa64.efi` | `usr/lib/grub/<grub arch>-efi-signed/grubnet<efi arch>.efi.signed` of the deb `pool/main/g/grub2-signed/grub-efi-<deb arch>-signed_<version>_<deb arch>.deb` |
| kernel | `vmlinuz-<platform>` | `casper/vmlinuz` |
| initrd | `initrd-<platform>` | `casper/initrd` |

`<efi arch>` is `x64` or `aa64`, `<deb arch>` `amd64` or `arm64`, and `<grub arch>` `x86_64` or
`arm64`. The ISO's name is its served name. What was found when the pins were taken:

- **The shim and GRUB are Canonical's signed packages.** The two debs in each ISO's pool are the
  same files the Ubuntu archive lists in its signed package indexes (`noble` for `shim-signed`
  1.58, `noble-updates` for `grub2-signed` 1.202.5, on `archive.ubuntu.com` for amd64 and on
  `ports.ubuntu.com` for arm64), with the same SHA-256. So the two signed chains name the same
  bytes.
- **The shim is the one the ISO itself boots.** `shim<efi arch>.efi.signed.latest` is byte for
  byte the ISO's own first boot file (`EFI/boot/bootx64.efi`, `efi/boot/bootaa64.efi`). Its one
  signature is by the Microsoft Corporation UEFI CA 2011. The package's `.dualsigned` file adds a
  signature by Canonical's own CA, which server firmware does not trust by default, so it is not
  used.
- **The GRUB is the network GRUB.** `grubnet<efi arch>.efi.signed` is the GRUB built for network
  boot. The ISO's own `grub<efi arch>.efi` is the CD GRUB (`gcd<efi arch>.efi.signed`), which
  reads its configuration from the CD, not from the boot server.
- **The kernel and initrd are the ISO's default ones.** They are those of the ISO's first boot
  entry, the release's general availability kernel (6.8). The ISO also holds a hardware enablement
  pair, `casper/hwe-vmlinuz` and `casper/hwe-initrd`; a change of these two pins to those is a
  change of `iso_path`, `bytes` and `sha256`, nothing else. The kernel, the initrd and the live
  system the initrd loads from the ISO must come from the same ISO, since the live system's
  kernel modules match that one kernel.
- **Canonical's netboot directories are not a source.** `releases.ubuntu.com/24.04/netboot/` and
  `cdimage.ubuntu.com/.../release/netboot/`, and the `-netboot-` tarballs beside the ISOs, are in
  no `SHA256SUMS`. Their shim and GRUB are the same bytes as the pinned ones. The amd64 kernel
  and initrd there are the ISO's hardware enablement pair. The arm64 kernel and initrd there match
  neither pair of the arm64 ISO.

## Checking a changed pin

A change to `pins.toml` is checked against Canonical's signatures before it is merged, by the
person who makes it, and the pull request says so. Work in a scratch directory of the workspace,
not `/tmp`. For each platform:

1. Download the ISO at `url`, and `SHA256SUMS` and `SHA256SUMS.gpg` from `sums`.
2. Check the signature with the Ubuntu CD image key (fingerprint above): `gpg --verify
   SHA256SUMS.gpg SHA256SUMS` must say `Good signature` from that key.
3. The ISO's `sha256sum` must be the one `SHA256SUMS` lists for its name, and equal `sha256`
   of `iso`; its size must equal `bytes`.
4. Take each other file out of the ISO: `casper/vmlinuz` and `casper/initrd` as they are, and the
   two debs from the pool, each opened with `dpkg-deb -x` to read the file at `deb_path`. Each
   file's size and `sha256sum` must equal its pin.

`cargo test -p xtask` then renders the changed pins and refuses one of the wrong form. A pin that
is well formed but wrong is refused later, never served: `hostkeeper-pxe fetch` (item N8 of the
design, `crates/hostkeeper-pxe`) checks the size and SHA-256 of every file it takes against
`netboot.json`.
