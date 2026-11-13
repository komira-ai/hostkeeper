//! `Layout` of `hostkeeper.operator.v1`, as plain functions over its messages, as the other
//! operator calls are ([`crate::operator`]): the lab's layout the store keeps ([`crate::layout`];
//! `docs/design/ui-shell-lab.md`).
//!
//! Every call takes the operator token only: a valid CI token is `forbidden`, and no credential
//! `unauthorized`, before the request is looked at. `Get` changes nothing. A write is judged
//! first, on the layout as it is, so that a call that would be refused writes nothing and no audit
//! line; then audited (a call whose audit line cannot be written changes nothing:
//! `audit_unavailable`); then made under the store's own lock, which judges it again. A change the
//! store then cannot write is `store_failed` and leaves the audit entry that was written before it.
//!
//! The audit entry of a write is `layout_name_put`, `layout_row_add`, `layout_row_remove`,
//! `layout_rack_put`, `layout_rack_remove`, `layout_place` or `layout_unplace`, about the id of the row or rack, or
//! the subject's key, with the operator's reason, or "layout edit" when there is none.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::LayoutRefusal as Refusal;

use crate::audit_actions::about;
use crate::layout::{Kind, Layout, LayoutError, Placement, Rack, Row, Subject};
use crate::operator::{Caller, entry_proto, optional_reason};
use crate::ops::{AuditEntry, Write};
use crate::service::Coordinator;

/// The reason of a write that names none.
const DEFAULT_REASON: &str = "layout edit";

/// The audit action of `PutName`.
pub const LAYOUT_NAME_PUT: &str = "layout_name_put";
/// The audit action of `AddRow`.
pub const LAYOUT_ROW_ADD: &str = "layout_row_add";
/// The audit action of `RemoveRow`.
pub const LAYOUT_ROW_REMOVE: &str = "layout_row_remove";
/// The audit action of `PutRack`.
pub const LAYOUT_RACK_PUT: &str = "layout_rack_put";
/// The audit action of `RemoveRack`.
pub const LAYOUT_RACK_REMOVE: &str = "layout_rack_remove";
/// The audit action of `Place`, and of an enrollment's approval that places the host.
pub const LAYOUT_PLACE: &str = "layout_place";
/// The audit action of `Unplace`.
pub const LAYOUT_UNPLACE: &str = "layout_unplace";

impl From<LayoutError> for Refusal {
    fn from(why: LayoutError) -> Self {
        match why {
            LayoutError::Invalid(_) => Self::InvalidRequest,
            LayoutError::NameTaken(_) => Self::NameTaken,
            LayoutError::UnknownRow(_) => Self::UnknownRow,
            LayoutError::UnknownRack(_) => Self::UnknownRack,
            LayoutError::UnknownPlacement(_) => Self::UnknownPlacement,
            LayoutError::InUse(_) => Self::InUse,
            LayoutError::Overlap { .. } => Self::Overlap,
            LayoutError::OutOfRack { .. } => Self::OutOfRack,
            LayoutError::Capacity(_) => Self::Capacity,
            LayoutError::Io(_) | LayoutError::Stored { .. } => {
                tracing::error!(%why, "a change of the layout was not written: nothing changed");
                Self::StoreFailed
            }
        }
    }
}

fn row_proto(row: &Row) -> proto::LayoutRow {
    proto::LayoutRow {
        id: row.id.clone(),
        number: row.number,
    }
}

fn rack_proto(rack: &Rack) -> proto::LayoutRack {
    proto::LayoutRack {
        id: rack.id.clone(),
        row: rack.row.clone(),
        number: rack.number,
        height_u: rack.height_u,
    }
}

fn placement_proto(placement: &Placement) -> proto::LayoutPlacement {
    let kind = match placement.subject.kind {
        Kind::Host => proto::LayoutSubjectKind::Host,
        Kind::Inventory => proto::LayoutSubjectKind::Inventory,
    };
    proto::LayoutPlacement {
        subject: Some(proto::LayoutSubject {
            kind: kind.into(),
            id: placement.subject.id.clone(),
        }),
        rack: placement.rack.clone(),
        u: placement.u,
        height_u: placement.height_u,
    }
}

/// The subject `subject` names.
fn subject_of(subject: Option<&proto::LayoutSubject>) -> Result<Subject, Refusal> {
    let subject = subject.ok_or(Refusal::InvalidRequest)?;
    let kind = match proto::LayoutSubjectKind::try_from(subject.kind) {
        Ok(proto::LayoutSubjectKind::Host) => Kind::Host,
        Ok(proto::LayoutSubjectKind::Inventory) => Kind::Inventory,
        _ => return Err(Refusal::InvalidRequest),
    };
    Ok(Subject {
        kind,
        id: subject.id.clone(),
    })
}

/// The rack a new rack stands at height when the request names none: 42 U.
const DEFAULT_HEIGHT_U: u32 = 42;

/// The placement of `subject` in `rack`, from its lowest U `u`, `height_u` high.
pub(crate) fn placement_of(subject: Subject, rack: &str, u: u32, height_u: u32) -> Placement {
    Placement {
        subject,
        rack: rack.to_owned(),
        u,
        height_u,
    }
}

impl Coordinator {
    /// Makes the change `edit` of the layout, audited as `entry` first, and answers what it found
    /// with the entry as numbered.
    fn change_layout<T>(
        &self,
        entry: AuditEntry,
        edit: impl Fn(&Layout) -> Result<(Layout, T), LayoutError>,
    ) -> Result<(T, proto::AuditEntry), Refusal> {
        edit(&self.layout().snapshot())?;
        let numbered = self
            .ops
            .change(Write::BeforeMemory, entry, |_| {})
            .map_err(Refusal::from)?;
        let found = self.layout().change(edit)?;
        Ok((found, entry_proto(&numbered)))
    }

    /// The `layout_place` audit entry an approval's placement of the subject with `key` writes.
    pub(crate) fn place_entry(by: &str, key: &str) -> AuditEntry {
        let mut entry = about(by, LAYOUT_PLACE, key);
        entry.reason = Some("approval".to_owned());
        entry
    }

    /// The audit entry of `action` by `by` about `subject` for the request's `reason`.
    fn layout_entry(
        by: &str,
        action: &str,
        subject: &str,
        reason: &str,
    ) -> Result<AuditEntry, Refusal> {
        let reason = optional_reason(reason, Refusal::InvalidRequest)?;
        let mut entry = about(by, action, subject);
        entry.reason = Some(reason.unwrap_or_else(|| DEFAULT_REASON.to_owned()));
        Ok(entry)
    }

    /// `Layout.Get`: the whole layout. Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`.
    pub fn layout_get(
        &self,
        caller: &Caller,
        _request: &proto::GetLayoutRequest,
    ) -> Result<proto::GetLayoutResponse, Refusal> {
        caller.operator()?;
        let layout = self.layout().snapshot();
        Ok(proto::GetLayoutResponse {
            name: layout.name().to_owned(),
            rows: layout.rows().into_iter().map(row_proto).collect(),
            racks: layout.racks().into_iter().map(rack_proto).collect(),
            placements: layout
                .placements()
                .into_iter()
                .map(placement_proto)
                .collect(),
        })
    }

    /// `Layout.PutName`: names the cluster.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request`, `store_failed`, `audit_unavailable`.
    pub fn layout_put_name(
        &self,
        caller: &Caller,
        request: &proto::PutLayoutNameRequest,
    ) -> Result<proto::PutLayoutNameResponse, Refusal> {
        let by = caller.operator()?;
        let entry = Self::layout_entry(by, LAYOUT_NAME_PUT, &request.name, &request.reason)?;
        let (name, entry) = self.change_layout(entry, |held| {
            held.with_name(&request.name)
                .map(|next| (next, request.name.clone()))
        })?;
        Ok(proto::PutLayoutNameResponse {
            name,
            entry: Some(entry),
        })
    }

    /// `Layout.AddRow`: adds the next row.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request`, `capacity`, `store_failed`,
    /// `audit_unavailable`.
    pub fn layout_add_row(
        &self,
        caller: &Caller,
        request: &proto::AddLayoutRowRequest,
    ) -> Result<proto::AddLayoutRowResponse, Refusal> {
        let by = caller.operator()?;
        let entry = Self::layout_entry(by, LAYOUT_ROW_ADD, "row", &request.reason)?;
        let (row, entry) = self.change_layout(entry, Layout::with_next_row)?;
        Ok(proto::AddLayoutRowResponse {
            row: Some(row_proto(&row)),
            entry: Some(entry),
        })
    }

    /// `Layout.RemoveRow`: removes a row that holds no rack.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request`, `unknown_row`, `in_use`, `store_failed`,
    /// `audit_unavailable`.
    pub fn layout_remove_row(
        &self,
        caller: &Caller,
        request: &proto::RemoveLayoutRowRequest,
    ) -> Result<proto::RemoveLayoutRowResponse, Refusal> {
        let by = caller.operator()?;
        let entry = Self::layout_entry(by, LAYOUT_ROW_REMOVE, &request.id, &request.reason)?;
        let ((), entry) = self.change_layout(entry, |held| {
            held.without_row(&request.id).map(|next| (next, ()))
        })?;
        Ok(proto::RemoveLayoutRowResponse { entry: Some(entry) })
    }

    /// `Layout.PutRack`: adds a rack to a row, or resizes or moves one.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request`, `unknown_rack`, `unknown_row`,
    /// `out_of_rack`, `overlap`, `capacity`, `store_failed`, `audit_unavailable`.
    pub fn layout_put_rack(
        &self,
        caller: &Caller,
        request: &proto::PutLayoutRackRequest,
    ) -> Result<proto::PutLayoutRackResponse, Refusal> {
        let by = caller.operator()?;
        let about = if request.id.is_empty() {
            "rack"
        } else {
            &request.id
        };
        let entry = Self::layout_entry(by, LAYOUT_RACK_PUT, about, &request.reason)?;
        let height_u = match (request.height_u, request.id.is_empty()) {
            (0, true) => DEFAULT_HEIGHT_U,
            (height, _) => height,
        };
        let (rack, entry) = self.change_layout(entry, |held| {
            held.with_rack(&request.id, &request.row, height_u)
        })?;
        Ok(proto::PutLayoutRackResponse {
            rack: Some(rack_proto(&rack)),
            entry: Some(entry),
        })
    }

    /// `Layout.RemoveRack`: removes a rack that holds no placement.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request`, `unknown_rack`, `in_use`, `store_failed`,
    /// `audit_unavailable`.
    pub fn layout_remove_rack(
        &self,
        caller: &Caller,
        request: &proto::RemoveLayoutRackRequest,
    ) -> Result<proto::RemoveLayoutRackResponse, Refusal> {
        let by = caller.operator()?;
        let entry = Self::layout_entry(by, LAYOUT_RACK_REMOVE, &request.id, &request.reason)?;
        let ((), entry) = self.change_layout(entry, |held| {
            held.without_rack(&request.id).map(|next| (next, ()))
        })?;
        Ok(proto::RemoveLayoutRackResponse { entry: Some(entry) })
    }

    /// `Layout.Place`: puts a subject in a rack, or moves it there.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request`, `unknown_rack`, `out_of_rack`, `overlap`,
    /// `capacity`, `store_failed`, `audit_unavailable`.
    pub fn layout_place(
        &self,
        caller: &Caller,
        request: &proto::PlaceLayoutRequest,
    ) -> Result<proto::PlaceLayoutResponse, Refusal> {
        let by = caller.operator()?;
        let subject = subject_of(request.subject.as_ref())?;
        let entry = Self::layout_entry(by, LAYOUT_PLACE, &subject.key(), &request.reason)?;
        let placement = placement_of(subject, &request.rack, request.u, request.height_u);
        let (placement, entry) =
            self.change_layout(entry, |held| held.with_placement(placement.clone()))?;
        Ok(proto::PlaceLayoutResponse {
            placement: Some(placement_proto(&placement)),
            entry: Some(entry),
        })
    }

    /// `Layout.Unplace`: takes a subject out of its rack.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request`, `unknown_placement`, `store_failed`,
    /// `audit_unavailable`.
    pub fn layout_unplace(
        &self,
        caller: &Caller,
        request: &proto::UnplaceLayoutRequest,
    ) -> Result<proto::UnplaceLayoutResponse, Refusal> {
        let by = caller.operator()?;
        let subject = subject_of(request.subject.as_ref())?;
        let entry = Self::layout_entry(by, LAYOUT_UNPLACE, &subject.key(), &request.reason)?;
        let ((), entry) = self.change_layout(entry, |held| {
            held.without_placement(&subject).map(|next| (next, ()))
        })?;
        Ok(proto::UnplaceLayoutResponse { entry: Some(entry) })
    }
}
