//! The tests of `serve`. The answer set and the inventory are tested as pure functions; the run
//! is tested end to end: `serve` started through its own command line, on loopback sockets the
//! kernel gives ports to, reading a fake coordinator over TLS, and spoken to as a machine's
//! firmware, GRUB and installer would, by DHCP, TFTP and HTTP.

mod fake;
mod kit;

mod proxy;
mod reads;
mod router;
