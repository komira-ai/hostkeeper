//! The plans one pass over a fleet reads more than once, kept while a [`Memo`] stands: each
//! service's plan at its desired version ([`current`]), and whether other services' rollouts hold it
//! ([`elsewhere`]). Moved out of [`crate::stages`], which reads them.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::live::live_elsewhere;
use crate::services::{ServiceDef, Services};
use crate::stages::View;

/// Whether `service` may not start a rollout now: its own is not live and as many other
/// services' are as `--live-rollouts` lets be.
pub(crate) fn elsewhere(
    services: &Services,
    all: &BTreeMap<String, Desired>,
    service: &str,
    fleet: &Fleet,
    now: Duration,
) -> bool {
    let name = memo_name(services.get(service).and_then(ServiceDef::group), service);
    let kept = MEMO.with(|memo| {
        memo.borrow()
            .as_ref()
            .map(|plans| plans.elsewhere.get(&name).copied())
    });
    match kept {
        Some(Some(held)) => held,
        Some(None) => {
            let held = live_elsewhere(services, all, service, fleet, now).is_some();
            MEMO.with(|memo| {
                memo.borrow_mut()
                    .as_mut()
                    .map(|plans| plans.elsewhere.insert(name, held))
            });
            held
        }
        None => live_elsewhere(services, all, service, fleet, now).is_some(),
    }
}

/// The name a [`Memo`] keeps a service's plan under: the service's own, or, for one made for a host
/// group, the group's name before it, so that two groups' plans of one package are two.
fn memo_name(group: Option<&crate::host_groups::GroupName>, service: &str) -> String {
    group.map_or_else(|| service.to_owned(), |group| format!("{group}/{service}"))
}

thread_local! {
    /// The plans [`current`] built while a [`Memo`] stands, by service and desired version.
    static MEMO: RefCell<Option<Plans>> = const { RefCell::new(None) };
}

/// What a [`Memo`] keeps: each service's plan by service and desired version (the service's name
/// is its group's before it, for a service made for a group), and whether another service's
/// rollout holds it ([`elsewhere`]).
#[derive(Default)]
struct Plans {
    views: BTreeMap<(String, String), Rc<View>>,
    elsewhere: BTreeMap<String, bool>,
}

/// While it stands, each service's plan is built once at its desired version and read again from
/// memory ([`current`]): for a pass over a fleet that does not change under it, such as the
/// decisions of one heartbeat until it is ordered something, or a status. The pass begins one
/// and drops it before it changes the fleet. A memo begun while another stands leaves that one.
pub(crate) struct Memo {
    began: bool,
}

impl Memo {
    /// A memo for this thread, unless one stands already.
    #[must_use]
    pub(crate) fn begin() -> Self {
        let began = MEMO.with(|memo| {
            let mut memo = memo.borrow_mut();
            let began = memo.is_none();
            if began {
                *memo = Some(Plans::default());
            }
            began
        });
        Self { began }
    }
}

impl Drop for Memo {
    fn drop(&mut self) {
        if self.began {
            MEMO.with(|memo| *memo.borrow_mut() = None);
        }
    }
}

/// The plan of `def` at its desired version `want` as the decision reads it
/// ([`View::effective`]), from the memo while one stands.
pub(crate) fn current(def: &ServiceDef, want: &Desired, fleet: &Fleet, now: Duration) -> Rc<View> {
    let build = || {
        Rc::new(View::effective(
            def,
            want,
            fleet.rollout_in(def),
            fleet,
            now,
        ))
    };
    MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        let Some(plans) = memo.as_mut() else {
            return build();
        };
        let key = (
            memo_name(def.group(), &def.name),
            want.version.as_str().to_owned(),
        );
        Rc::clone(plans.views.entry(key).or_insert_with(build))
    })
}
