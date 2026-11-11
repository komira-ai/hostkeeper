//! When a Mac is pushed (`docs/design/coordinator-mdm.md`, section 4.3), as a state machine over
//! injected times, so a test sets the clock and no code here reads one.
//!
//! The rules are the ones the model check of `hostkeeper-mdm-model` (`QueueModel`, CM4) holds with
//! no violation, and each has a test named for it:
//!
//! - A push is *outstanding* for an enrollment from the push until **any request from that
//!   enrollment reaches `/mdm`** ([`PushPolicy::mac_connected`]): an event, not a timer. A
//!   command queued while one is outstanding sends no second push; one queued after the Mac
//!   answered is pushed at once, whatever the floor says.
//! - Every push arms the **floor**, `push_retry_s` after it. When the floor elapses and a command
//!   is still queued, the Mac is pushed again and the floor is armed again: a push repeated for
//!   a Mac that has not connected, and never inside the window.
//! - `NotNow` arms the floor ([`PushPolicy::command_deferred`]), so a deferred command, which is
//!   back in the queue with the Mac's connection about to close, has a later push.
//! - A command a timeout returned to the queue is pushed again
//!   ([`PushPolicy::command_timed_out`]): the Mac may have dropped the connection that held it.
//!
//! The queue (CM11a) says which enrollments have a queued command; this keeps only the pushes.

use std::collections::BTreeMap;

/// What the policy keeps of one enrollment.
#[derive(Clone, Copy, Debug, Default)]
struct State {
    /// A push was sent and no request of the enrollment has reached `/mdm` since.
    outstanding: bool,
    /// When the floor elapses, if it is armed.
    floor_at: Option<u64>,
}

/// The push policy of every enrollment.
#[derive(Clone, Debug)]
pub struct PushPolicy {
    retry_s: u64,
    states: BTreeMap<String, State>,
}

impl PushPolicy {
    /// A policy whose floor is `retry_s` seconds (the setting `push_retry_s`).
    #[must_use]
    pub fn new(retry_s: u64) -> Self {
        Self {
            retry_s,
            states: BTreeMap::new(),
        }
    }

    /// Changes the floor, for the pushes armed from now on (a setting was changed).
    pub fn set_retry_s(&mut self, retry_s: u64) {
        self.retry_s = retry_s;
    }

    /// A command was queued for `id` at `now`. Whether to push now: when no push is outstanding.
    pub fn command_queued(&mut self, id: &str, now: u64) -> bool {
        if self.states.get(id).is_some_and(|state| state.outstanding) {
            return false;
        }
        self.push(id, now);
        true
    }

    /// A command of `id` that a timeout returned to the queue, at `now`: pushed again, always.
    pub fn command_timed_out(&mut self, id: &str, now: u64) -> bool {
        self.push(id, now);
        true
    }

    /// `id` answered a command `NotNow`, at `now`: the floor is armed, if it is not already, so
    /// that the command is pushed once the window has passed.
    pub fn command_deferred(&mut self, id: &str, now: u64) {
        let retry = self.retry_s;
        let state = self.states.entry(id.to_owned()).or_default();
        state.floor_at.get_or_insert(now.saturating_add(retry));
    }

    /// A request of `id` reached `/mdm`: the push, if one was outstanding, was answered.
    pub fn mac_connected(&mut self, id: &str) {
        if let Some(state) = self.states.get_mut(id) {
            state.outstanding = false;
        }
    }

    /// The push of `id` could not be sent: nothing is outstanding, and the floor stays armed, so
    /// the push is tried again once the window has passed.
    pub fn push_failed(&mut self, id: &str) {
        if let Some(state) = self.states.get_mut(id) {
            state.outstanding = false;
        }
    }

    /// The floor of `id` elapsed at `now` (`has_queued`: a command is still queued): whether to
    /// push again. A floor not yet due answers no and stays armed.
    pub fn floor_elapsed(&mut self, id: &str, now: u64, has_queued: bool) -> bool {
        let due = self
            .states
            .get(id)
            .and_then(|state| state.floor_at)
            .is_some_and(|at| at <= now);
        if !due {
            return false;
        }
        if let Some(state) = self.states.get_mut(id) {
            state.floor_at = None;
        }
        if has_queued {
            self.push(id, now);
        }
        has_queued
    }

    /// The enrollments to push again at `now`: those whose floor elapsed and for which
    /// `has_queued` says a command is queued.
    pub fn due(&mut self, now: u64, has_queued: &dyn Fn(&str) -> bool) -> Vec<String> {
        let ids: Vec<String> = self.states.keys().cloned().collect();
        ids.into_iter()
            .filter(|id| self.floor_elapsed(id, now, has_queued(id)))
            .collect()
    }

    /// When the first armed floor elapses, for the timer that calls [`Self::due`].
    #[must_use]
    pub fn next_floor(&self) -> Option<u64> {
        self.states
            .values()
            .filter_map(|state| state.floor_at)
            .min()
    }

    /// Drops what is kept of `id`: the enrollment is gone.
    pub fn forget(&mut self, id: &str) {
        self.states.remove(id);
    }

    /// Whether a push is outstanding for `id`.
    #[must_use]
    pub fn is_outstanding(&self, id: &str) -> bool {
        self.states.get(id).is_some_and(|state| state.outstanding)
    }

    /// A push of `id` is sent at `now`: outstanding, and the floor armed after it.
    fn push(&mut self, id: &str, now: u64) {
        let floor = now.saturating_add(self.retry_s);
        let state = self.states.entry(id.to_owned()).or_default();
        state.outstanding = true;
        state.floor_at = Some(floor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: &str = "mac-1";

    #[test]
    fn a_command_queued_with_no_push_outstanding_is_pushed_at_once() {
        let mut policy = PushPolicy::new(3600);
        assert!(policy.command_queued(MAC, 100));
        assert!(policy.is_outstanding(MAC));
        assert_eq!(policy.next_floor(), Some(3700));
    }

    #[test]
    fn a_second_command_while_a_push_is_outstanding_sends_no_second_push() {
        let mut policy = PushPolicy::new(3600);
        assert!(policy.command_queued(MAC, 100));
        assert!(!policy.command_queued(MAC, 101));
        assert!(!policy.command_queued(MAC, 3_000));
    }

    #[test]
    fn a_command_after_an_answered_push_is_pushed_at_once_inside_the_floor() {
        let mut policy = PushPolicy::new(3600);
        assert!(policy.command_queued(MAC, 100));
        // The Mac connected: the push was answered, though the floor is far off.
        policy.mac_connected(MAC);
        assert!(!policy.is_outstanding(MAC));
        assert!(policy.command_queued(MAC, 700));
        assert_eq!(policy.next_floor(), Some(4300));
    }

    #[test]
    fn outstanding_is_cleared_by_a_connection_and_not_by_the_passing_of_time() {
        let mut policy = PushPolicy::new(10);
        assert!(policy.command_queued(MAC, 0));
        // A long time passes with no request from the Mac: still outstanding.
        assert!(!policy.command_queued(MAC, 1_000_000));
        assert!(policy.is_outstanding(MAC));
    }

    #[test]
    fn a_floor_that_elapsed_with_a_queued_command_pushes_again_and_arms_the_floor_again() {
        let mut policy = PushPolicy::new(3600);
        assert!(policy.command_queued(MAC, 100));
        assert!(!policy.floor_elapsed(MAC, 3_699, true));
        assert!(policy.floor_elapsed(MAC, 3_700, true));
        assert_eq!(policy.next_floor(), Some(7300));
    }

    #[test]
    fn a_floor_that_elapsed_with_nothing_queued_sends_no_push_and_disarms() {
        let mut policy = PushPolicy::new(3600);
        assert!(policy.command_queued(MAC, 0));
        assert!(!policy.floor_elapsed(MAC, 3_600, false));
        assert_eq!(policy.next_floor(), None);
    }

    #[test]
    fn not_now_arms_the_push_floor_so_the_command_gets_a_later_push_and_none_inside_it() {
        let mut policy = PushPolicy::new(3600);
        // The Mac answered a push, took a command and said NotNow: nothing is outstanding.
        assert!(policy.command_queued(MAC, 0));
        policy.mac_connected(MAC);
        policy.command_deferred(MAC, 50);
        assert_eq!(policy.next_floor(), Some(3_600));
        // The floor armed by the push at 0 is the earlier; a second NotNow does not move it.
        policy.command_deferred(MAC, 60);
        assert_eq!(policy.next_floor(), Some(3_600));
        assert!(!policy.floor_elapsed(MAC, 3_599, true));
        assert!(policy.floor_elapsed(MAC, 3_600, true));
    }

    #[test]
    fn not_now_on_an_enrollment_with_no_floor_arms_one_from_the_answer() {
        let mut policy = PushPolicy::new(3600);
        policy.command_deferred(MAC, 1_000);
        assert_eq!(policy.next_floor(), Some(4_600));
        assert!(!policy.is_outstanding(MAC));
        assert!(!policy.floor_elapsed(MAC, 4_599, true));
        assert!(policy.floor_elapsed(MAC, 4_600, true));
    }

    #[test]
    fn a_timed_out_command_is_pushed_again_even_with_a_push_outstanding() {
        let mut policy = PushPolicy::new(3600);
        assert!(policy.command_queued(MAC, 0));
        assert!(policy.command_timed_out(MAC, 86_400));
        assert!(policy.is_outstanding(MAC));
        assert_eq!(policy.next_floor(), Some(90_000));
    }

    #[test]
    fn a_push_that_failed_leaves_the_floor_armed_and_nothing_outstanding() {
        let mut policy = PushPolicy::new(3600);
        assert!(policy.command_queued(MAC, 0));
        policy.push_failed(MAC);
        assert!(!policy.is_outstanding(MAC));
        assert_eq!(policy.next_floor(), Some(3_600));
        // A new command is pushed at once; the failure was not an answer, but nothing waits.
        assert!(policy.command_queued(MAC, 10));
        // A failure of an enrollment the policy does not know changes nothing.
        policy.push_failed("unknown");
        policy.mac_connected("unknown");
        assert!(!policy.is_outstanding("unknown"));
    }

    #[test]
    fn due_names_the_enrollments_whose_floor_elapsed_with_a_command_queued() {
        let mut policy = PushPolicy::new(100);
        assert!(policy.command_queued("a", 0));
        assert!(policy.command_queued("b", 0));
        assert!(policy.command_queued("c", 50));
        let queued = |id: &str| id != "b";
        assert!(policy.due(99, &queued).is_empty());
        assert_eq!(policy.due(100, &queued), vec!["a".to_owned()]);
        // b's floor elapsed with nothing queued and is gone; c's is next.
        assert_eq!(policy.next_floor(), Some(150).min(Some(200)));
        assert_eq!(policy.due(150, &queued), vec!["c".to_owned()]);
    }

    #[test]
    fn the_floor_follows_a_changed_setting_from_the_next_push() {
        let mut policy = PushPolicy::new(100);
        assert!(policy.command_queued(MAC, 0));
        policy.set_retry_s(10);
        assert_eq!(policy.next_floor(), Some(100));
        policy.mac_connected(MAC);
        assert!(policy.command_queued(MAC, 1_000));
        assert_eq!(policy.next_floor(), Some(1_010));
    }

    #[test]
    fn a_forgotten_enrollment_keeps_nothing() {
        let mut policy = PushPolicy::new(100);
        assert!(policy.command_queued(MAC, 0));
        policy.forget(MAC);
        assert_eq!(policy.next_floor(), None);
        assert!(!policy.is_outstanding(MAC));
        assert!(policy.command_queued(MAC, 5));
    }

    #[test]
    fn a_floor_of_a_huge_window_does_not_overflow() {
        let mut policy = PushPolicy::new(u64::MAX);
        assert!(policy.command_queued(MAC, 10));
        assert_eq!(policy.next_floor(), Some(u64::MAX));
    }
}
