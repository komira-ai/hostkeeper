# Package proxy (superseded)

Status: **removed with the coordinator's MDM** (2026-11-14). This document designed a caching
pass-through that the coordinator would serve to the lab LAN, at `https://<mdm name>/apt/<repo>/`
and `/pkg/<sha256>.pkg`, so that Macs, wiped hosts and bulk updates would use the lab network.

It was built, and it was served on one listener only: the MDM listener (`Surface::Mdm`), which
supplied the public-CA certificate apt requires of an origin. The MDM listener is gone, so the
proxy has nowhere to run: the flag `--proxy-cache-dir`, the proxy's cache and its routes were
removed with it. It was never deployed (the coordinator's stack on the control plane never passed
`--proxy-cache-dir`).

Hosts and Macs fetch packages from the public repository (`packages.komira.ai`) as before. If a
lab-LAN cache is wanted again, it is new design work: a listener that holds a certificate a public
CA issued, and the decisions this document made again, from its history in the repository.
