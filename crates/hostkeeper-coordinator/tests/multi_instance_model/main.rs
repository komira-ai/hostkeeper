//! The models of `docs/design/coordinator-multi-instance-checks.md`, section 14: the coordinator's
//! protocol for N instances on one Postgres (the gate row taken with `SKIP LOCKED` by beats, the
//! conditional writes, the ledger retry, the ordered beat upsert, the quiet rule, the periodic
//! claim, the migration mutex, the one-snapshot cache reload, the audit counter and the one lock
//! order), each searched exhaustively over every interleaving of its statements at N = 1, 2 and 3
//! instances.
//!
//! The spec is `docs/design/models/coordinator-multi-instance.md`: it states each model's bound,
//! every invariant (each the name of a test here) and every variant (a lock or a conditional write
//! taken away) with the invariant it breaks and the smallest N at which it does.
//! `the_spec_states_every_invariant_variant_and_bound_of_the_models` holds the spec to the models
//! below, both ways. The test of an invariant searches the locked protocol of its model at N = 1,
//! 2 and 3, which must keep every invariant of the model, and then each of its variants, which
//! must break it from the stated N on and keep it below.
//!
//! Pure: no database, no clock, no thread, no file but the spec, read at compile time. The
//! models are the protocol, not the SQL; the statements the design names are the steps.

mod audit;
mod beats;
mod drain;
mod explore;
mod harness;
mod ledger;
mod locks;
mod migrate;
mod operator;
mod orders;
mod periodic;
mod quiet;
mod reload;
mod steps;

use harness::ModelDef;

/// Every model, in the order of the spec.
const MODELS: &[&ModelDef] = &[
    &orders::MODEL,
    &operator::MODEL,
    &steps::MODEL,
    &quiet::MODEL,
    &ledger::MODEL,
    &beats::MODEL,
    &reload::MODEL,
    &audit::MODEL,
    &periodic::MODEL,
    &migrate::MODEL,
    &locks::MODEL,
    &drain::MODEL,
];

/// The spec the models are held to.
const SPEC: &str = include_str!("../../../../docs/design/models/coordinator-multi-instance.md");

/// One test per invariant, each named as the invariant, and the list of their names.
macro_rules! invariant_tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                harness::case(MODELS, stringify!($name));
            }
        )*
        const TESTS: &[&str] = &[$(stringify!($name)),*];
    };
}

invariant_tests! {
    // orders
    n_instances_decide_one_rollout_at_a_time,
    queue_follows_set_seq_on_both_tasks,
    stage_never_has_more_updating_hosts_than_max_updating,
    min_up_holds_in_every_stage,
    one_host_never_has_two_orders,
    order_after_crash_is_returned_again,
    a_host_behind_gets_an_order_whichever_instance_hears_it,
    // operator
    acknowledged_halt_is_not_outrun_by_an_order_of_the_other_task,
    acknowledged_hold_and_desired_change_are_read_by_the_next_decision,
    beat_never_overtakes_a_queued_operator_edit,
    // steps
    stage_advance_by_racing_instances_is_one_advance_and_one_audit_line,
    halt_raised_by_two_instances_is_one_generation_and_one_audit_line,
    rollback_is_committed_before_the_uninstall_is_ordered,
    // quiet
    no_order_after_a_gap_in_beats_until_quiet_ends,
    partial_fleet_in_quiet_window_raises_no_halt_and_reverts_nothing,
    operator_halt_in_quiet_window_is_read_by_every_decision,
    quiet_ends_once_every_host_has_beaten,
    a_decision_after_the_operator_halt_can_still_wait_on_it,
    // ledger
    unknown_outcome_commit_lands_once_by_retry,
    settled_outcome_is_what_landed,
    // beats
    stale_beat_does_not_overwrite_a_newer_one,
    host_whose_clock_stepped_back_is_believed_again,
    // reload
    cache_is_never_labelled_newer_than_its_content,
    host_behind_the_desired_version_is_ordered_after_a_reload,
    // audit
    audit_numbers_follow_commit_order,
    new_release_again_continues_audit_numbers_after_the_release_before_ran,
    set_seq_of_a_row_changed_by_the_release_before_is_restamped_in_queue_order,
    // periodic
    periodic_job_runs_once_per_period_with_n_claimers,
    claimer_death_loses_one_run_only,
    alert_is_logged_once,
    // migrate
    two_starters_apply_each_migration_once,
    every_migration_is_applied_once_a_starter_survives,
    // locks
    lock_order_has_no_cycle,
    // drain
    no_connection_is_refused_at_the_edge_during_an_update,
}

#[test]
fn the_spec_states_every_invariant_variant_and_bound_of_the_models() {
    if let Err(problems) = harness::compare(SPEC, MODELS, TESTS) {
        panic!("the spec and the models differ:\n{problems}");
    }
}

#[test]
fn a_spec_that_leaves_out_a_variant_or_changes_its_n_is_refused() {
    let line = "| `decision_without_the_gate` | `orders` | `min_up_holds_in_every_stage` | 3 |";
    assert!(
        SPEC.contains(line),
        "the spec holds the row this test edits"
    );

    let dropped = SPEC.replace(&format!("{line}\n"), "");
    let problems = harness::compare(&dropped, MODELS, TESTS).expect_err("a row is missing");
    assert_eq!(
        problems,
        "a model defines a variant (name, model, breaks, fails from N) the spec does not state: \
         (\"decision_without_the_gate\", \"orders\", \"min_up_holds_in_every_stage\", \"3\")"
    );

    let changed = SPEC.replace(line, &line.replace("| 3 |", "| 2 |"));
    let problems = harness::compare(&changed, MODELS, TESTS).expect_err("a row differs");
    assert_eq!(
        problems,
        "the spec states a variant (name, model, breaks, fails from N) no model defines: \
         (\"decision_without_the_gate\", \"orders\", \"min_up_holds_in_every_stage\", \"2\")\n\
         a model defines a variant (name, model, breaks, fails from N) the spec does not state: \
         (\"decision_without_the_gate\", \"orders\", \"min_up_holds_in_every_stage\", \"3\")"
    );
}

#[test]
fn a_spec_or_test_list_that_leaves_out_an_invariant_is_refused() {
    let without_test: Vec<&str> = TESTS
        .iter()
        .copied()
        .filter(|t| *t != "min_up_holds_in_every_stage")
        .collect();
    let problems = harness::compare(SPEC, MODELS, &without_test).expect_err("a test is missing");
    assert_eq!(
        problems,
        "the invariant min_up_holds_in_every_stage has no test"
    );

    let line = "| `min_up_holds_in_every_stage` | `orders` |";
    let start = SPEC.find(line).expect("the spec states the invariant");
    let end = start + SPEC[start..].find('\n').expect("a whole row") + 1;
    let dropped = format!("{}{}", &SPEC[..start], &SPEC[end..]);
    let problems = harness::compare(&dropped, MODELS, TESTS).expect_err("a row is missing");
    assert_eq!(
        problems,
        "a model defines an invariant (name, model) the spec does not state: \
         (\"min_up_holds_in_every_stage\", \"orders\")"
    );
}
