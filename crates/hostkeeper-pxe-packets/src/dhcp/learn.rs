//! Router-mode learning: which client took which address, and its architecture.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use crate::{ClientArch, Mac};

/// The most MACs, and the most addresses, kept in one run. The farm LAN holds far fewer
/// clients; past this, the entry learned longest ago is dropped first, so a flood of made-up
/// MACs or addresses costs memory up to this bound and no more, and a client that boots after
/// it is still learned.
pub const MAX_LEARNED: usize = 4096;

/// What a REQUEST taught: the client `mac` takes `address`, and states `arch` (from the
/// REQUEST's option 93, or else the last DISCOVER with its MAC), or no architecture at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease {
    /// The address the router offered (the REQUEST's option 50).
    pub address: Ipv4Addr,
    /// The client (`chaddr`).
    pub mac: Mac,
    /// Its architecture, when it stated one.
    pub arch: Option<ClientArch>,
}

/// A map of at most [`MAX_LEARNED`] entries, which drops the entry inserted longest ago.
#[derive(Clone, Debug)]
struct Bounded<K, V> {
    entries: BTreeMap<K, (u64, V)>,
    order: BTreeMap<u64, K>,
    next: u64,
}

impl<K, V> Default for Bounded<K, V> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            order: BTreeMap::new(),
            next: 0,
        }
    }
}

impl<K: Ord + Copy, V: Copy> Bounded<K, V> {
    fn insert(&mut self, key: K, value: V) {
        if let Some((seq, _)) = self.entries.remove(&key) {
            self.order.remove(&seq);
        }
        self.entries.insert(key, (self.next, value));
        self.order.insert(self.next, key);
        self.next += 1;
        if self.entries.len() > MAX_LEARNED {
            let oldest = self.order.pop_first();
            let _ = oldest.and_then(|(_, key)| self.entries.remove(&key));
        }
    }

    fn get(&self, key: &K) -> Option<V> {
        self.entries.get(key).map(|&(_, value)| value)
    }
}

/// What router mode learned in this run.
#[derive(Clone, Debug, Default)]
pub(crate) struct Learner {
    /// The architecture of the last DISCOVER of each MAC that stated one.
    discovered: Bounded<Mac, ClientArch>,
    /// The last REQUEST that named each address.
    leases: Bounded<Ipv4Addr, Lease>,
}

impl Learner {
    /// A DISCOVER from `mac`: its architecture, when it states one, is kept.
    pub(crate) fn discover(&mut self, mac: Mac, arch: Option<ClientArch>) {
        if let Some(arch) = arch {
            self.discovered.insert(mac, arch);
        }
    }

    /// A REQUEST from `mac` for `address` (its option 50) stating `arch`: the lease it teaches,
    /// or `None` when it names no address.
    pub(crate) fn request(
        &mut self,
        mac: Mac,
        address: Option<Ipv4Addr>,
        arch: Option<ClientArch>,
    ) -> Option<Lease> {
        let address = address?;
        let lease = Lease {
            address,
            mac,
            arch: arch.or_else(|| self.discovered.get(&mac)),
        };
        self.leases.insert(address, lease);
        Some(lease)
    }

    /// The lease learned last for `address`.
    pub(crate) fn lease(&self, address: Ipv4Addr) -> Option<Lease> {
        self.leases.get(&address)
    }
}
