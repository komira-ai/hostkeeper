# Held scripts

The scripts in this directory are run by the repository's owner, by hand, from an operator box. No
workflow, test or other script runs them. Each one:

- changes nothing by default: it makes its reads and prints each change it would make as
  `DRY-RUN: <command>`, and acts only with `--apply`;
- prints every call before it makes it: `READ:` a read, `+` a change (a read repeated while it
  waits, once);
- never forces: it passes no flag that overrides a refusal (such as `--take-last-host`), and with
  `--apply` stops at the first refusal or failure rather than going on to the next change;
- takes its settings as flags, and reaches the coordinator through `hk` and its settings file
  (`hk settings init`, [docs/cli.md](../../cli.md)), so it holds no address, token or path of its
  own;
- writes files only into a directory it is given (`--record`), never under `/tmp`, and never
  overwrites or removes one;
- is POSIX sh, run with `sh <script>`, and `sh -n` and `bash -n` clean.

| Script | What |
|---|---|
| [xcode-install.sh](xcode-install.sh) | Install an Xcode release the store holds on farm Macs, one Mac at a time, each in one maintenance run of the kind `xcode` followed to its end, with the run's audit entries ([maintenance.md](../maintenance.md#bringing-in-a-new-xcode)) |
