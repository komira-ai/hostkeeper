//! A client's hardware address, as DHCP's `chaddr` carries it.

use std::fmt;

/// An Ethernet hardware address: the `chaddr` of a DHCP packet whose `htype` is 1 and `hlen`
/// is 6. Shown as six lower-case hex pairs separated by `:`, the form the inventory shows a
/// record's data MACs in.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mac([u8; 6]);

impl Mac {
    /// The address of these six octets.
    #[must_use]
    pub const fn new(octets: [u8; 6]) -> Self {
        Self(octets)
    }

    /// The six octets.
    #[must_use]
    pub const fn octets(self) -> [u8; 6] {
        self.0
    }
}

impl fmt::Display for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}

impl fmt::Debug for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Mac({self})")
    }
}
