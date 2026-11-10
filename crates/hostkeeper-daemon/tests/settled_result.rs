//! The verdict an update of hostkeeper itself keeps to report ([`settled_result`]): only a
//! finished record that kept an order id has one.

mod support;

use hostkeeper_daemon::link_adapters::settled_result;
use hostkeeper_hook::Verdict;
use hostkeeper_steps::record::State;
use support::selfupdate::record_of;

/// A record that is not finished has no settled verdict to report, whatever it keeps.
#[test]
fn an_unfinished_record_has_no_settled_result() {
    let mut record = record_of("hostkeeper", State::Pending);
    record.progress.report_order_id = Some("order-1".to_owned());
    assert_eq!(settled_result(&record), None);
}

/// A finished record that kept no order id has none either.
#[test]
fn a_finished_record_that_kept_no_order_id_has_no_settled_result() {
    let record = record_of(
        "hostkeeper",
        State::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false,
            },
            reason: None,
        },
    );
    assert_eq!(settled_result(&record), None);
}
