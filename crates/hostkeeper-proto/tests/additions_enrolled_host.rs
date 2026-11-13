//! `InventoryRecord.enrolled_host_id` (field 17), added after the last release: the host id of the
//! host enrolled against the record. A record written before it reads with it empty; a reader
//! before it reads every field it knew from a record that carries it and keeps the field, so a
//! release before the field enrolls, lists and matches as it did.

use prost::Message as _;

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// `InventoryRecord` as it was before the field: fields 1 to 16 of which this test carries the
/// ones the Inventory page read.
#[derive(Clone, PartialEq, prost::Message)]
struct RecordBefore {
    #[prost(string, tag = "1")]
    host_id: String,
    #[prost(string, repeated, tag = "3")]
    data_macs: Vec<String>,
    #[prost(string, tag = "4")]
    bmc_mac: String,
    #[prost(string, tag = "6")]
    notes: String,
    #[prost(bool, tag = "9")]
    enrolled: bool,
}

/// Proves: a record the release before wrote (no field 17) reads with every field it carried and
/// no enrolled host; a record that carries the field reads back the host id; and a reader of the
/// release before reads the fields it knew from it.
#[test]
fn a_record_before_the_enrolled_host_reads_with_none_and_an_older_reader_reads_the_rest() {
    let before = RecordBefore {
        host_id: "db-1".to_owned(),
        data_macs: vec!["0c:c4:7a:00:00:02".to_owned()],
        bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
        notes: "rack 4".to_owned(),
        enrolled: true,
    };
    let read = op::InventoryRecord::decode(before.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.host_id, "db-1");
    assert!(read.enrolled);
    assert_eq!(read.enrolled_host_id, "");

    let now = op::InventoryRecord {
        host_id: "db-1".to_owned(),
        data_macs: vec!["0c:c4:7a:00:00:02".to_owned()],
        bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
        notes: "rack 4".to_owned(),
        enrolled: true,
        enrolled_host_id: "web-9".to_owned(),
        ..op::InventoryRecord::default()
    };
    let bytes = now.encode_to_vec();
    assert_eq!(
        op::InventoryRecord::decode(bytes.as_slice())
            .expect("decodes")
            .enrolled_host_id,
        "web-9"
    );
    assert_eq!(
        RecordBefore::decode(bytes.as_slice()).expect("an older reader decodes"),
        before
    );
}
