//! `Layout` of the operator API ([`crate::operator_layout`]): each call's answer and audit entry,
//! each refusal with nothing changed and no audit line, the operator-only access, and the status
//! the gRPC adapter makes of a refusal. No clock and no timing.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::LayoutRefusal as Refusal;

use crate::host_groups_support::{World, coordinator_listed};
use crate::layout::{Layout, LayoutStore};
use crate::marks_support::operator;
use crate::operator::Caller;
use crate::ops::AuditEntry;
use crate::service::Coordinator;
use crate::store::{audit_failure, store_failure};

/// A world whose coordinator keeps its layout in the world's store, so a failing store fails the
/// layout's commits too.
fn world(name: &str) -> World {
    let mut world = World::new(name);
    world.rig.coordinator = coordinator_listed(
        &world.fake,
        &world.rig.clock,
        Some(Arc::clone(&world.enrollment.enrollment)),
        "",
    )
    .with_layout(LayoutStore::over(
        Arc::new(world.fake.clone()),
        Layout::default(),
    ));
    world
}

fn host(id: &str) -> Option<proto::LayoutSubject> {
    Some(proto::LayoutSubject {
        kind: proto::LayoutSubjectKind::Host.into(),
        id: id.to_owned(),
    })
}

fn place(
    subject: Option<proto::LayoutSubject>,
    rack: &str,
    u: u32,
    h: u32,
) -> proto::PlaceLayoutRequest {
    proto::PlaceLayoutRequest {
        subject,
        rack: rack.to_owned(),
        u,
        height_u: h,
        ..proto::PlaceLayoutRequest::default()
    }
}

fn get(c: &Coordinator) -> proto::GetLayoutResponse {
    c.layout_get(&operator(), &proto::GetLayoutRequest {})
        .expect("get")
}

fn rack_of(row: &str, height_u: u32) -> proto::PutLayoutRackRequest {
    proto::PutLayoutRackRequest {
        row: row.to_owned(),
        height_u,
        ..proto::PutLayoutRackRequest::default()
    }
}

fn audit_actions(entries: &[AuditEntry]) -> Vec<(String, Option<String>)> {
    entries
        .iter()
        .map(|e| (e.action.clone(), e.subject.clone()))
        .collect()
}

fn unplace(id: &str) -> proto::UnplaceLayoutRequest {
    proto::UnplaceLayoutRequest {
        subject: host(id),
        reason: String::new(),
    }
}

fn remove_row(id: &str) -> proto::RemoveLayoutRowRequest {
    proto::RemoveLayoutRowRequest {
        id: id.to_owned(),
        reason: String::new(),
    }
}

fn remove_rack(id: &str) -> proto::RemoveLayoutRackRequest {
    proto::RemoveLayoutRackRequest {
        id: id.to_owned(),
        reason: String::new(),
    }
}

/// Proves: the calls build a layout and read it back in order: the cluster is named, rows are added
/// as 1 and 2, racks are added to a row with the next number and 42 U unless a height is named, a
/// rack is resized, a server is placed, moved and unplaced, and each write answers its item and an
/// audit entry named for it, with the reason "layout edit" or the operator's own.
///
/// Catches: a write that is not audited, or an audit entry about the wrong id.
#[test]
fn the_calls_build_the_layout_and_each_write_is_audited() {
    let world = world("layout-calls");
    let c = world.coordinator();
    let op = operator();
    assert!(get(c).rows.is_empty() && get(c).name.is_empty());

    let named = c
        .layout_put_name(
            &op,
            &proto::PutLayoutNameRequest {
                name: "Lab 1".into(),
                reason: "first".into(),
            },
        )
        .expect("named");
    assert_eq!(named.name, "Lab 1");
    assert_eq!(named.entry.expect("entry").reason.as_deref(), Some("first"));

    let add = || c.layout_add_row(&op, &proto::AddLayoutRowRequest::default());
    let one = add().expect("row").row.expect("row");
    let two = add().expect("row").row.expect("row");
    assert_eq!((one.id.as_str(), one.number, two.number), ("row-1", 1, 2));

    let default = c.layout_put_rack(&op, &rack_of("row-1", 0)).expect("rack");
    let default = default.rack.expect("rack");
    assert_eq!(
        (default.id.as_str(), default.number, default.height_u),
        ("rack-1", 1, 42)
    );
    let second = c.layout_put_rack(&op, &rack_of("row-1", 24)).expect("rack");
    assert_eq!(second.rack.expect("rack").number, 2);
    let resized = c
        .layout_put_rack(
            &op,
            &proto::PutLayoutRackRequest {
                id: "rack-2".into(),
                height_u: 20,
                ..proto::PutLayoutRackRequest::default()
            },
        )
        .expect("resized");
    assert_eq!(resized.rack.expect("rack").height_u, 20);

    let placed = c
        .layout_place(&op, &place(host("n1"), "rack-1", 41, 2))
        .expect("placed");
    let p = placed.placement.expect("placement");
    assert_eq!((p.u, p.height_u, p.rack.as_str()), (41, 2, "rack-1"));
    let entry = Some(proto::LayoutSubject {
        kind: proto::LayoutSubjectKind::Inventory.into(),
        id: "db-1".to_owned(),
    });
    let inventory = c
        .layout_place(&op, &place(entry, "rack-1", 10, 1))
        .expect("an inventory entry placed");
    assert_eq!(
        inventory.placement.expect("placement").subject,
        Some(proto::LayoutSubject {
            kind: proto::LayoutSubjectKind::Inventory.into(),
            id: "db-1".to_owned(),
        })
    );
    let moved = c
        .layout_place(&op, &place(host("n1"), "rack-2", 1, 1))
        .expect("moved");
    assert_eq!(moved.placement.expect("placement").rack, "rack-2");
    let read = get(c);
    assert_eq!(
        (
            read.name.as_str(),
            read.rows.len(),
            read.racks.len(),
            read.placements.len()
        ),
        ("Lab 1", 2, 2, 2)
    );
    c.layout_unplace(&op, &unplace("n1")).expect("unplaced");
    c.layout_remove_rack(&op, &remove_rack("rack-2"))
        .expect("rack removed");
    let removed = c
        .layout_remove_row(&op, &remove_row("row-2"))
        .expect("row removed");
    assert_eq!(
        removed.entry.expect("entry").reason.as_deref(),
        Some("layout edit")
    );
    assert_eq!(
        audit_actions(&world.audit()),
        [
            ("layout_name_put", "Lab 1"),
            ("layout_row_add", "row"),
            ("layout_row_add", "row"),
            ("layout_rack_put", "rack"),
            ("layout_rack_put", "rack"),
            ("layout_rack_put", "rack-2"),
            ("layout_place", "host:n1"),
            ("layout_place", "inventory:db-1"),
            ("layout_place", "host:n1"),
            ("layout_unplace", "host:n1"),
            ("layout_rack_remove", "rack-2"),
            ("layout_row_remove", "row-2"),
        ]
        .map(|(a, s)| (a.to_owned(), Some(s.to_owned())))
    );
}

/// Proves: a placement that overlaps another, one past the rack's top, and one with a U or height of
/// 0 are refused as `overlap`, `out_of_rack` and `invalid_request`, a row or a rack that holds
/// something is `in_use`, and each refusal changes nothing and writes no audit line.
///
/// Catches: a refusal that leaves half a change, or an audit line for a change that did not happen.
#[test]
fn a_refused_write_changes_nothing_and_leaves_no_audit_line() {
    let world = world("layout-refusals");
    let c = world.coordinator();
    let op = operator();
    c.layout_add_row(&op, &proto::AddLayoutRowRequest::default())
        .expect("row");
    c.layout_put_rack(&op, &rack_of("row-1", 10)).expect("rack");
    c.layout_place(&op, &place(host("a"), "rack-1", 3, 2))
        .expect("placed");
    let before = (get(c), world.audit());

    let placing =
        |request: proto::PlaceLayoutRequest| c.layout_place(&op, &request).expect_err("refused");
    assert_eq!(placing(place(host("b"), "rack-1", 4, 2)), Refusal::Overlap);
    assert_eq!(
        placing(place(host("b"), "rack-1", 10, 2)),
        Refusal::OutOfRack
    );
    assert_eq!(
        placing(place(host("b"), "rack-9", 1, 1)),
        Refusal::UnknownRack
    );
    assert_eq!(
        placing(place(host("b"), "rack-1", 0, 1)),
        Refusal::InvalidRequest
    );
    assert_eq!(
        placing(place(host("b"), "rack-1", 1, 0)),
        Refusal::InvalidRequest
    );
    assert_eq!(
        placing(place(None, "rack-1", 1, 1)),
        Refusal::InvalidRequest
    );
    let nokind = Some(proto::LayoutSubject {
        kind: 0,
        id: "x".into(),
    });
    assert_eq!(
        placing(place(nokind, "rack-1", 1, 1)),
        Refusal::InvalidRequest
    );
    let mut reasoned = place(host("b"), "rack-1", 1, 1);
    reasoned.reason = "x".repeat(300);
    assert_eq!(placing(reasoned), Refusal::InvalidRequest);

    assert_eq!(
        c.layout_remove_row(&op, &remove_row("row-1"))
            .expect_err("a row with a rack"),
        Refusal::InUse
    );
    assert_eq!(
        c.layout_remove_rack(&op, &remove_rack("rack-1"))
            .expect_err("a rack with a server"),
        Refusal::InUse
    );
    let shorter = c
        .layout_put_rack(
            &op,
            &proto::PutLayoutRackRequest {
                id: "rack-1".into(),
                height_u: 3,
                ..proto::PutLayoutRackRequest::default()
            },
        )
        .expect_err("shorter than its server");
    assert_eq!(shorter, Refusal::OutOfRack);
    assert_eq!(
        c.layout_unplace(&op, &unplace("zz"))
            .expect_err("not placed"),
        Refusal::UnknownPlacement
    );
    assert_eq!(
        c.layout_remove_row(&op, &remove_row("row-7"))
            .expect_err("no such row"),
        Refusal::UnknownRow
    );
    assert_eq!(
        c.layout_put_rack(&op, &rack_of("row-7", 5))
            .expect_err("no row"),
        Refusal::UnknownRow
    );
    assert_eq!(
        c.layout_put_name(
            &op,
            &proto::PutLayoutNameRequest {
                name: "<b>".into(),
                reason: String::new()
            }
        )
        .expect_err("a bad name"),
        Refusal::InvalidRequest
    );
    assert_eq!((get(c), world.audit()), before);
}

/// Proves: the layout holds at most 64 rows, and the 65th `AddRow` is `capacity`, with nothing
/// added.
///
/// Catches: a layout grown without bound through the API.
#[test]
fn the_sixty_fifth_row_is_refused_as_capacity() {
    let world = world("layout-capacity");
    let c = world.coordinator();
    for _ in 0..64 {
        c.layout_add_row(&operator(), &proto::AddLayoutRowRequest::default())
            .expect("row");
    }
    let refused = c
        .layout_add_row(&operator(), &proto::AddLayoutRowRequest::default())
        .expect_err("65th");
    assert_eq!(refused, Refusal::Capacity);
    assert_eq!(get(c).rows.len(), 64);
}

/// Proves: only the operator's token reaches `Layout`: a CI token is `forbidden` and no credential
/// `unauthorized`, on each of the eight calls, and nothing changes.
///
/// Catches: a call that skips the credential check.
#[test]
fn only_the_operator_reaches_the_layout() {
    let world = world("layout-access");
    let c = world.coordinator();
    for (caller, wanted) in [
        (Caller::Ci, Refusal::Forbidden),
        (Caller::Anonymous, Refusal::Unauthorized),
    ] {
        let refused = [
            c.layout_get(&caller, &proto::GetLayoutRequest {}).err(),
            c.layout_put_name(&caller, &proto::PutLayoutNameRequest::default())
                .err(),
            c.layout_add_row(&caller, &proto::AddLayoutRowRequest::default())
                .err(),
            c.layout_remove_row(&caller, &proto::RemoveLayoutRowRequest::default())
                .err(),
            c.layout_put_rack(&caller, &proto::PutLayoutRackRequest::default())
                .err(),
            c.layout_remove_rack(&caller, &proto::RemoveLayoutRackRequest::default())
                .err(),
            c.layout_place(&caller, &proto::PlaceLayoutRequest::default())
                .err(),
            c.layout_unplace(&caller, &proto::UnplaceLayoutRequest::default())
                .err(),
            c.layout_put_device(&caller, &proto::PutLayoutDeviceRequest::default())
                .err(),
            c.layout_remove_device(&caller, &proto::RemoveLayoutDeviceRequest::default())
                .err(),
        ];
        assert!(refused.iter().all(|r| *r == Some(wanted)), "{refused:?}");
    }
    assert!(get(c).rows.is_empty() && world.audit().is_empty());
}

/// Proves: a commit that fails is `store_failed` and one whose audit line cannot be kept
/// `audit_unavailable`, each leaving the layout as it was; once the store takes commits again the
/// same call lands.
///
/// Catches: a layout in memory that the store does not hold.
#[test]
fn a_commit_that_fails_changes_nothing_and_a_resend_lands() {
    let world = world("layout-commit-fails");
    let c = world.coordinator();
    for (failure, refusal) in [
        (store_failure as fn() -> _, Refusal::StoreFailed),
        (audit_failure, Refusal::AuditUnavailable),
    ] {
        world.fake.refuse(move |_| Some(failure()));
        let refused = c
            .layout_add_row(&operator(), &proto::AddLayoutRowRequest::default())
            .expect_err("refused");
        assert_eq!(refused, refusal);
        assert!(get(c).rows.is_empty());
    }
    world.fake.heal();
    c.layout_add_row(&operator(), &proto::AddLayoutRowRequest::default())
        .expect("lands");
    assert_eq!(get(c).rows.len(), 1);
}

/// Proves: the gRPC adapter answers each refusal with the status code its enum documents and the
/// token as the message, so the page and `hkctl` read the same word.
///
/// Catches: a refusal that reaches the client as a bare `Unknown`.
#[test]
fn each_refusal_is_a_status_with_its_code_and_token() {
    use tonic::Code::{
        AlreadyExists, FailedPrecondition, Internal, InvalidArgument, NotFound, PermissionDenied,
        Unauthenticated, Unavailable,
    };
    for (refusal, code, token) in [
        (Refusal::Unspecified, Internal, "unspecified"),
        (Refusal::StoreFailed, Internal, "store_failed"),
        (Refusal::Unauthorized, Unauthenticated, "unauthorized"),
        (Refusal::Forbidden, PermissionDenied, "forbidden"),
        (Refusal::InvalidRequest, InvalidArgument, "invalid_request"),
        (Refusal::AuditUnavailable, Unavailable, "audit_unavailable"),
        (Refusal::UnknownRow, NotFound, "unknown_row"),
        (Refusal::UnknownRack, NotFound, "unknown_rack"),
        (Refusal::UnknownPlacement, NotFound, "unknown_placement"),
        (Refusal::UnknownDevice, NotFound, "unknown_device"),
        (Refusal::NameTaken, AlreadyExists, "name_taken"),
        (Refusal::InUse, FailedPrecondition, "in_use"),
        (Refusal::Overlap, FailedPrecondition, "overlap"),
        (Refusal::OutOfRack, FailedPrecondition, "out_of_rack"),
        (Refusal::Capacity, FailedPrecondition, "capacity"),
    ] {
        let status = crate::grpc::refusal::layout(refusal);
        assert_eq!(status.code(), code, "{token}");
        if refusal != Refusal::Unspecified {
            assert_eq!(status.message(), token);
        }
    }
}

/// Proves: each error of the layout is the refusal of the call it is: a name or number taken, a
/// store that cannot write and a stored layout that cannot be read are `name_taken` and
/// `store_failed`, each other error its own token.
///
/// Catches: an error that reaches the operator as another token.
#[test]
fn each_error_of_the_layout_is_a_refusal() {
    use crate::layout::LayoutError as E;
    let io = || {
        E::Io(crate::desired::StoreError::Io {
            path: "fake".into(),
            source: std::io::Error::other("no"),
        })
    };
    let stored = E::Stored {
        path: "p".into(),
        why: "w".into(),
    };
    for (error, wanted) in [
        (E::NameTaken("row 1".into()), Refusal::NameTaken),
        (io(), Refusal::StoreFailed),
        (stored, Refusal::StoreFailed),
        (
            E::UnknownPlacement("host:a".into()),
            Refusal::UnknownPlacement,
        ),
        (E::UnknownDevice("net.1".into()), Refusal::UnknownDevice),
    ] {
        assert_eq!(Refusal::from(error), wanted);
    }
}

fn switch(name: &str, role: proto::NetworkRole) -> proto::PutLayoutDeviceRequest {
    proto::PutLayoutDeviceRequest {
        name: name.to_owned(),
        role: role.into(),
        ..proto::PutLayoutDeviceRequest::default()
    }
}

/// Proves: a network device is added, read back by `Get` (by name) with every field, replaced by its
/// id, placed with `Place` under the inventory subject of its id, and removed together with its
/// placement; each write answers the device and an audit entry named `layout_device_put` or
/// `layout_device_remove` about `device` (an add) or the id, with the operator's reason or "layout edit".
///
/// Catches: a device the page cannot read back, a placement left behind by a removed device, or a
/// write that is not audited.
#[test]
fn a_network_device_is_added_placed_replaced_and_removed_with_its_placement() {
    let world = world("layout-devices");
    let c = world.coordinator();
    let op = operator();
    c.layout_add_row(&op, &proto::AddLayoutRowRequest::default())
        .expect("row");
    c.layout_put_rack(&op, &rack_of("row-1", 24)).expect("rack");

    let added = c
        .layout_put_device(
            &op,
            &proto::PutLayoutDeviceRequest {
                console_host: "node-3".into(),
                console_device: "/dev/ttyUSB0".into(),
                management_mac: "AA-BB-CC-DD-EE-FF".into(),
                notes: "uplink 1".into(),
                reason: "new switch".into(),
                ..switch("bmc-switch", proto::NetworkRole::Bmc)
            },
        )
        .expect("added");
    let device = added.device.expect("the device");
    assert_eq!(
        (device.id.as_str(), device.name.as_str(), device.role()),
        ("net.1", "bmc-switch", proto::NetworkRole::Bmc)
    );
    assert_eq!(
        (
            device.console_host.as_str(),
            device.console_device.as_str(),
            device.management_mac.as_str(),
            device.notes.as_str()
        ),
        ("node-3", "/dev/ttyUSB0", "aa:bb:cc:dd:ee:ff", "uplink 1")
    );
    assert_eq!(
        added.entry.expect("entry").reason.as_deref(),
        Some("new switch")
    );
    let data = c
        .layout_put_device(&op, &switch("data-switch", proto::NetworkRole::Data))
        .expect("a second")
        .device
        .expect("the device");
    assert_eq!(data.id, "net.2");
    let names: Vec<String> = get(c).devices.iter().map(|d| d.name.clone()).collect();
    assert_eq!(names, ["bmc-switch", "data-switch"]);

    let subject = Some(proto::LayoutSubject {
        kind: proto::LayoutSubjectKind::Inventory.into(),
        id: "net.1".to_owned(),
    });
    c.layout_place(&op, &place(subject.clone(), "rack-1", 24, 1))
        .expect("placed");
    let replaced = c
        .layout_put_device(
            &op,
            &proto::PutLayoutDeviceRequest {
                id: "net.1".into(),
                ..switch("core-bmc", proto::NetworkRole::Bmc)
            },
        )
        .expect("replaced")
        .device
        .expect("the device");
    assert_eq!(
        (replaced.name.as_str(), replaced.console_host.as_str()),
        ("core-bmc", "")
    );
    assert_eq!(
        get(c).placements.len(),
        1,
        "a replaced device keeps its place"
    );

    let removed = c
        .layout_remove_device(
            &op,
            &proto::RemoveLayoutDeviceRequest {
                id: "net.1".into(),
                reason: String::new(),
            },
        )
        .expect("removed");
    assert_eq!(
        removed.entry.expect("entry").reason.as_deref(),
        Some("layout edit")
    );
    let read = get(c);
    assert_eq!((read.devices.len(), read.placements.len()), (1, 0));
    assert_eq!(
        audit_actions(&world.audit())[2..],
        [
            ("layout_device_put", "device"),
            ("layout_device_put", "device"),
            ("layout_place", "inventory:net.1"),
            ("layout_device_put", "net.1"),
            ("layout_device_remove", "net.1"),
        ]
        .map(|(a, s)| (a.to_owned(), Some(s.to_owned())))
    );
}

/// Proves: a device with no role or a role this build does not know, a bad field, a taken name and
/// an id the layout does not hold are `invalid_request`, `invalid_request`, `name_taken` and
/// `unknown_device`, each changing nothing and writing no audit line; a reason with a control
/// character is `invalid_request` before anything is judged.
///
/// Catches: a refused device that is half kept, or an audit line for a change that did not happen.
#[test]
fn a_refused_device_changes_nothing_and_writes_no_audit_line() {
    let world = world("layout-devices-refused");
    let c = world.coordinator();
    let op = operator();
    c.layout_put_device(&op, &switch("sw", proto::NetworkRole::Data))
        .expect("added");
    let lines = world.audit().len();
    let put = |request: proto::PutLayoutDeviceRequest| c.layout_put_device(&op, &request);
    assert_eq!(
        put(proto::PutLayoutDeviceRequest {
            name: "x".into(),
            ..proto::PutLayoutDeviceRequest::default()
        })
        .expect_err("no role"),
        Refusal::InvalidRequest
    );
    assert_eq!(
        put(proto::PutLayoutDeviceRequest {
            name: "x".into(),
            role: 77,
            ..proto::PutLayoutDeviceRequest::default()
        })
        .expect_err("an unknown role"),
        Refusal::InvalidRequest
    );
    assert_eq!(
        put(switch("", proto::NetworkRole::Bmc)).expect_err("no name"),
        Refusal::InvalidRequest
    );
    assert_eq!(
        put(switch("sw", proto::NetworkRole::Bmc)).expect_err("taken"),
        Refusal::NameTaken
    );
    assert_eq!(
        put(proto::PutLayoutDeviceRequest {
            id: "net.9".into(),
            ..switch("other", proto::NetworkRole::Bmc)
        })
        .expect_err("unknown"),
        Refusal::UnknownDevice
    );
    assert_eq!(
        put(proto::PutLayoutDeviceRequest {
            reason: "a\u{7}".into(),
            ..switch("other", proto::NetworkRole::Bmc)
        })
        .expect_err("a reason"),
        Refusal::InvalidRequest
    );
    assert_eq!(
        c.layout_remove_device(
            &op,
            &proto::RemoveLayoutDeviceRequest {
                id: "net.9".into(),
                reason: String::new(),
            },
        )
        .expect_err("unknown"),
        Refusal::UnknownDevice
    );
    assert_eq!(get(c).devices.len(), 1);
    assert_eq!(world.audit().len(), lines);
}
