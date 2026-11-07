//! Who a gRPC call must be, checked before the call is read: a heartbeat by a host's client
//! certificate, every operator method by the operator's token, `SetDesired` and `Converge` also
//! by CI's token, and `Enroll`, `Poll` and `Renew` by nothing at the router.

use std::collections::{BTreeSet, HashMap};

use hostkeeper_proto::ctl::COMMANDS;
use hostkeeper_proto::hostkeeper::operator::v1::{
    ConvergeRequest, ConvergeResponse, DesiredState, SetDesiredRequest, SetDesiredResponse,
    StatusResponse,
};
use hyper::HeaderMap;
use hyper::header::{AUTHORIZATION, HeaderValue};
use tonic::Code;

use super::support::{
    CONVERGE, HEARTBEAT, SET_DESIRED, Served, call, decode, grpc_request, peer, served,
    served_built, served_unreachable, served_with_operator_token, updater,
};
use crate::grpc::{Admitted, admit};
use crate::http::Surface;
use crate::operator::Caller;
use crate::service::MAX_BEARER_BYTES;
use crate::testutil::{OPERATOR, Rig, ci_token, with_kbf};

/// The path of `Releases.Status`, an operator method with a handler.
const STATUS: &str = "/hostkeeper.operator.v1.Releases/Status";

/// A second service the coordinator manages and CI's allow-list does not name.
const OTHER_SERVICE: &str = "[[service]]\nname = \"other\"\nroles = [\"daemon\"]\n";

/// The path of each method of the operator API.
fn operator_paths() -> Vec<&'static str> {
    COMMANDS
        .iter()
        .filter(|command| command.package == "hostkeeper.operator.v1")
        .map(|command| command.path)
        .collect()
}

/// The request headers of a caller with `token` as its bearer.
fn headers(token: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(token) = token {
        let value = HeaderValue::from_str(&format!("Bearer {token}")).expect("a header value");
        headers.insert(AUTHORIZATION, value);
    }
    headers
}

/// What the credential step made of a caller.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The caller is admitted.
    Admitted,
    /// The caller is refused with this code and this message.
    Refused(Code, String),
}

fn refused(code: Code, message: &str) -> Outcome {
    Outcome::Refused(code, message.to_owned())
}

/// The outcome of the credential step of `path` on the private listener for a caller with
/// `token` and no certificate.
async fn outcome(served: &Served, path: &str, token: Option<&str>) -> Outcome {
    match admit(
        &served.coordinator,
        Surface::Control,
        path,
        &None,
        &headers(token),
    )
    .await
    {
        Ok(_) => Outcome::Admitted,
        Err(status) => Outcome::Refused(status.code(), status.message().to_owned()),
    }
}

/// A `SetDesired` of kbf's version 2.0.
fn set_kbf() -> SetDesiredRequest {
    SetDesiredRequest {
        service: "kbf".to_owned(),
        desired: Some(DesiredState {
            version: "2.0".to_owned(),
            min_up: HashMap::from([("storage".to_owned(), 1), ("frontend".to_owned(), 1)]),
            ..DesiredState::default()
        }),
        ..SetDesiredRequest::default()
    }
}

/// Proves: an operator method refuses a caller that has only a host certificate, only a bad
/// token, or nothing: `UNAUTHENTICATED` with the message `unauthorized`, before the method runs
/// (so for a method with no handler too).
#[tokio::test]
async fn operator_methods_refuse_a_host_certificate_and_no_token() {
    let served = served("grpc-cred-no-token");
    let host = updater("host-1", "storage");
    for path in operator_paths() {
        for (certificate, token) in [(&host, None), (&None, None), (&host, Some("not-a-token"))] {
            let request = grpc_request(path, (), token);
            let answer = call(&served.coordinator, Surface::Control, certificate, request).await;
            assert_eq!(
                answer,
                (Code::Unauthenticated, "unauthorized".to_owned(), None),
                "{path} with token {token:?}"
            );
        }
    }
}

/// Proves: the operator token is no credential for a heartbeat, which takes a client
/// certificate that names an updater: with the token and no certificate, or a certificate that
/// names no updater, the call is `PERMISSION_DENIED` with the message `forbidden`; with an
/// updater's certificate the token plays no part and the call is not refused as `forbidden`.
#[tokio::test]
async fn heartbeat_refuses_an_operator_token() {
    let served = served("grpc-cred-heartbeat");
    let stranger = peer(&["dns:not-an-updater"]);
    for (certificate, token) in [
        (&None, Some(OPERATOR.to_owned())),
        (&stranger, Some(OPERATOR.to_owned())),
        (&None, Some(ci_token())),
        (&None, None),
    ] {
        let request = grpc_request(HEARTBEAT, (), token.as_deref());
        let answer = call(
            &served.coordinator,
            Surface::Heartbeat,
            certificate,
            request,
        )
        .await;
        assert_eq!(
            answer,
            (Code::PermissionDenied, "forbidden".to_owned(), None),
            "token {token:?}"
        );
    }
    let host = updater("host-1", "storage");
    let request = grpc_request(HEARTBEAT, (), Some(OPERATOR));
    let (code, message, _) = call(&served.coordinator, Surface::Heartbeat, &host, request).await;
    assert_ne!(
        (code, message.as_str()),
        (Code::PermissionDenied, "forbidden"),
        "an updater's certificate admits the heartbeat"
    );
}

/// Proves: `Enroll`, `Poll` and `Renew` take no credential at the router: with no certificate,
/// or a bearer that is nothing, the step admits the call and carries the peer's certificate
/// (none here) for `Renew` to judge.
#[tokio::test]
async fn enroll_poll_and_renew_take_no_credential_at_the_router() {
    let served = served("grpc-cred-enroll");
    for method in ["Enroll", "Poll", "Renew"] {
        let path = format!("/hostkeeper.agent.v1.Coordinator/{method}");
        let admitted = admit(
            &served.coordinator,
            Surface::Heartbeat,
            &path,
            &None,
            &headers(Some("not-a-token")),
        )
        .await
        .expect("admitted");
        assert_eq!(format!("{admitted:?}"), "Peer(None)", "{path}");
    }
}

/// Proves: CI's token reaches `SetDesired` and `Converge` and no other method: `Hosts.List`
/// and `Releases.Force` refuse it as `PERMISSION_DENIED` `forbidden` (and `Hosts.List`
/// answers the operator's token `OK`, so the refusal is the token's), `SetDesired` and
/// `Converge` answer CI's token, and a
/// `SetDesired` of a service that CI's allow-list entry does not name is `PERMISSION_DENIED`
/// `forbidden` and changes nothing.
#[tokio::test]
async fn ci_token_reaches_only_set_desired_and_converge() {
    let served = served("grpc-cred-ci");
    for path in [
        "/hostkeeper.operator.v1.Hosts/List",
        "/hostkeeper.operator.v1.Releases/Force",
    ] {
        let request = grpc_request(path, (), Some(&ci_token()));
        let answer = call(&served.coordinator, Surface::Control, &None, request).await;
        assert_eq!(
            answer,
            (Code::PermissionDenied, "forbidden".to_owned(), None),
            "{path}"
        );
    }
    // The refusal is the CI token's: the operator's token is answered on the same path.
    let request = grpc_request("/hostkeeper.operator.v1.Hosts/List", (), Some(OPERATOR));
    let (code, message, _) = call(&served.coordinator, Surface::Control, &None, request).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));

    let request = grpc_request(SET_DESIRED, set_kbf(), Some(&ci_token()));
    let (code, message, body) = call(&served.coordinator, Surface::Control, &None, request).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let set: SetDesiredResponse = decode(&body.expect("a message"));
    let desired = set.desired.expect("the state as stored");
    assert_eq!(desired.version, "2.0");
    assert_eq!(
        desired.min_up,
        HashMap::from([("storage".to_owned(), 1), ("frontend".to_owned(), 1)])
    );

    let request = grpc_request(CONVERGE, ConvergeRequest::default(), Some(&ci_token()));
    let (code, message, body) = call(&served.coordinator, Surface::Control, &None, request).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let converge: ConvergeResponse = decode(&body.expect("a message"));
    assert_eq!(
        converge.convergence.expect("a convergence").desired,
        Some("2.0".to_owned())
    );
}

/// Proves: a service the coordinator manages that CI's allow-list entry does not name is
/// refused `PERMISSION_DENIED` `forbidden`, and afterwards the status lists no service with a
/// desired state.
#[tokio::test]
async fn set_desired_of_a_service_off_the_allow_list_is_forbidden() {
    let served = served_built("grpc-cred-off-list", |dir, clock| {
        Rig::start(dir, clock).with_services(with_kbf(OTHER_SERVICE))
    });
    let request = SetDesiredRequest {
        service: "other".to_owned(),
        desired: Some(DesiredState {
            version: "3.0".to_owned(),
            min_up: HashMap::from([("storage".to_owned(), 0), ("frontend".to_owned(), 0)]),
            ..DesiredState::default()
        }),
        ..SetDesiredRequest::default()
    };
    let request = grpc_request(SET_DESIRED, request, Some(&ci_token()));
    let answer = call(&served.coordinator, Surface::Control, &None, request).await;
    assert_eq!(
        answer,
        (Code::PermissionDenied, "forbidden".to_owned(), None)
    );

    let request = grpc_request(STATUS, (), Some(OPERATOR));
    let (code, _, body) = call(&served.coordinator, Surface::Control, &None, request).await;
    assert_eq!(code, Code::Ok);
    let status: StatusResponse = decode(&body.expect("a message"));
    assert!(
        status.services.iter().all(|one| one.desired.is_none()),
        "nothing was set: {status:?}"
    );
    assert_eq!(
        status
            .services
            .iter()
            .map(|one| one.service.as_str())
            .collect::<Vec<_>>(),
        ["kbf", "other"]
    );
}

/// Proves: for every method of the operator API the credentials that are admitted are exactly
/// these: the operator token for every method, CI's token for `SetDesired` and `Converge` only,
/// and no credential for any. The others are refused with `unauthorized` (nothing) or
/// `forbidden` (a CI token at a method that takes the operator's only).
#[tokio::test]
async fn every_operator_method_admits_exactly_its_credential() {
    let served = served("grpc-cred-table");
    let paths = operator_paths();
    let services: BTreeSet<&str> = COMMANDS
        .iter()
        .filter(|command| command.package == "hostkeeper.operator.v1")
        .map(|command| command.service)
        .collect();
    assert_eq!(
        services,
        BTreeSet::from([
            "Alerts",
            "Audit",
            "Enrollment",
            "Hosts",
            "Mdm",
            "Releases",
            "Rollouts",
            "Services"
        ])
    );
    assert!(paths.contains(&SET_DESIRED) && paths.contains(&CONVERGE));
    for path in paths {
        let ci_method = path == SET_DESIRED || path == CONVERGE;
        let unauthorized = refused(Code::Unauthenticated, "unauthorized");
        let with_ci = if ci_method {
            Outcome::Admitted
        } else {
            refused(Code::PermissionDenied, "forbidden")
        };
        assert_eq!(
            outcome(&served, path, Some(OPERATOR)).await,
            Outcome::Admitted,
            "{path} with the operator token"
        );
        assert_eq!(
            outcome(&served, path, Some(&ci_token())).await,
            with_ci,
            "{path} with CI's token"
        );
        assert_eq!(
            outcome(&served, path, None).await,
            unauthorized,
            "{path} with no token"
        );
    }
}

/// Proves: admitting the operator's token on `Converge` widened no other method of `Releases`:
/// through the router, each of `Force`, `UnmarkBad`, `ClearHalt`, `Status`, `MarkBad` and
/// `Halt` still refuses CI's token as `PERMISSION_DENIED` `forbidden` and a call with no token as
/// `UNAUTHENTICATED` `unauthorized`, and nothing was set afterwards.
#[tokio::test]
async fn every_other_releases_method_still_refuses_ci_and_no_token() {
    let served = served("grpc-cred-releases");
    let others: Vec<&str> = operator_paths()
        .into_iter()
        .filter(|path| path.starts_with("/hostkeeper.operator.v1.Releases/"))
        .filter(|path| *path != SET_DESIRED && *path != CONVERGE)
        .collect();
    assert_eq!(
        others,
        [
            "/hostkeeper.operator.v1.Releases/Force",
            "/hostkeeper.operator.v1.Releases/UnmarkBad",
            "/hostkeeper.operator.v1.Releases/ClearHalt",
            "/hostkeeper.operator.v1.Releases/Status",
            "/hostkeeper.operator.v1.Releases/MarkBad",
            "/hostkeeper.operator.v1.Releases/Halt",
        ]
    );
    for path in others {
        let with_ci = grpc_request(path, (), Some(&ci_token()));
        assert_eq!(
            call(&served.coordinator, Surface::Control, &None, with_ci).await,
            (Code::PermissionDenied, "forbidden".to_owned(), None),
            "{path} with CI's token"
        );
        let with_none = grpc_request(path, (), None);
        assert_eq!(
            call(&served.coordinator, Surface::Control, &None, with_none).await,
            (Code::Unauthenticated, "unauthorized".to_owned(), None),
            "{path} with no token"
        );
    }

    let request = grpc_request(STATUS, (), Some(OPERATOR));
    let (code, _, body) = call(&served.coordinator, Surface::Control, &None, request).await;
    assert_eq!(code, Code::Ok);
    let status: StatusResponse = decode(&body.expect("a message"));
    assert!(
        status.services.iter().all(|one| one.desired.is_none()),
        "nothing was set: {status:?}"
    );
}

/// Proves: a bearer longer than the coordinator looks at counts as no bearer, though it is the
/// operator token: a token of exactly [`MAX_BEARER_BYTES`] is admitted, and one a byte longer
/// is `unauthorized`.
#[tokio::test]
async fn an_oversize_bearer_counts_as_absent() {
    let longest = "t".repeat(MAX_BEARER_BYTES);
    let served = served_with_operator_token("grpc-cred-longest", &longest);
    assert_eq!(
        outcome(&served, STATUS, Some(&longest)).await,
        Outcome::Admitted
    );

    let oversize = "t".repeat(MAX_BEARER_BYTES + 1);
    let served = served_with_operator_token("grpc-cred-oversize", &oversize);
    assert_eq!(
        outcome(&served, STATUS, Some(&oversize)).await,
        refused(Code::Unauthenticated, "unauthorized")
    );
}

/// Proves: only `Authorization: Bearer <token>` is a bearer: another scheme, a lower-case
/// scheme, a token with no scheme, a scheme with no token, and a header that is not text are
/// no token, each `unauthorized`; the same token after `Bearer ` is admitted.
#[tokio::test]
async fn an_authorization_header_that_is_not_a_bearer_is_no_token() {
    let served = served("grpc-cred-malformed");
    let wrong: [&[u8]; 6] = [
        b"Basic b3BlcmF0b3I6c2VjcmV0",
        b"bearer operator-token-0123456789abcdef0123456789",
        b"operator-token-0123456789abcdef0123456789",
        b"Bearer",
        b"Bearer ",
        b"Bearer \xff\xfe",
    ];
    for value in wrong {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_bytes(value).expect("a header value"),
        );
        let answer = admit(
            &served.coordinator,
            Surface::Control,
            STATUS,
            &None,
            &headers,
        )
        .await
        .map(|_| ())
        .map_err(|status| (status.code(), status.message().to_owned()));
        assert_eq!(
            answer,
            Err((Code::Unauthenticated, "unauthorized".to_owned())),
            "{}",
            String::from_utf8_lossy(value)
        );
    }
    assert_eq!(
        outcome(&served, STATUS, Some(OPERATOR)).await,
        Outcome::Admitted
    );
}

/// Proves: when the issuer's keys cannot be fetched, CI's `SetDesired` and `Converge` are
/// `UNAVAILABLE` with the message `keys_unavailable`; an operator method does not take CI's
/// token and says `unauthorized`; and the operator's token needs no issuer.
#[tokio::test]
async fn unreachable_keys_are_unavailable_for_ci_only() {
    let served = served_unreachable("grpc-cred-keys");
    for path in [SET_DESIRED, CONVERGE] {
        let request = grpc_request(path, (), Some(&ci_token()));
        let answer = call(&served.coordinator, Surface::Control, &None, request).await;
        assert_eq!(
            answer,
            (Code::Unavailable, "keys_unavailable".to_owned(), None),
            "{path}"
        );
    }
    let request = grpc_request("/hostkeeper.operator.v1.Hosts/Abort", (), Some(&ci_token()));
    let answer = call(&served.coordinator, Surface::Control, &None, request).await;
    assert_eq!(
        answer,
        (Code::Unauthenticated, "unauthorized".to_owned(), None)
    );
    let request = grpc_request(STATUS, (), Some(OPERATOR));
    let (code, _, _) = call(&served.coordinator, Surface::Control, &None, request).await;
    assert_eq!(code, Code::Ok);
}

/// Proves: what the credential step admits prints as who it is and never as the bearer: the
/// operator's token is nowhere in the text of an `Admitted`, and each variant prints its name
/// and its facts (the host and whether it retires, the size of a certificate, CI's claims).
#[tokio::test]
async fn admitted_debug_never_prints_the_bearer() {
    let served = served("grpc-cred-debug");
    let coordinator = &served.coordinator;

    let operator = admit(
        coordinator,
        Surface::Control,
        STATUS,
        &None,
        &headers(Some(OPERATOR)),
    )
    .await
    .expect("admitted");
    let name = coordinator.operator(Some(OPERATOR)).expect("a name");
    let text = format!("{operator:?}");
    assert_eq!(
        text,
        format!("Operator {{ caller: {:?}, .. }}", Caller::Operator(name))
    );
    assert!(!text.contains(OPERATOR), "{text}");
    assert!(matches!(operator, Admitted::Operator { bearer, .. } if bearer == OPERATOR));

    let certificate = updater("host-1", "storage");
    let der = certificate.as_deref().expect("a certificate");
    let host = admit(
        coordinator,
        Surface::Heartbeat,
        HEARTBEAT,
        &certificate,
        &headers(Some(OPERATOR)),
    )
    .await
    .expect("admitted");
    let identity = coordinator.identify(der).expect("an updater");
    assert_eq!(
        format!("{host:?}"),
        format!("Host {{ identity: {identity:?}, retiring: false }}")
    );

    let peer = admit(
        coordinator,
        Surface::Heartbeat,
        "/hostkeeper.agent.v1.Coordinator/Renew",
        &certificate,
        &headers(None),
    )
    .await
    .expect("admitted");
    assert_eq!(format!("{peer:?}"), format!("Peer(Some({}))", der.len()));

    let ci = admit(
        coordinator,
        Surface::Control,
        SET_DESIRED,
        &None,
        &headers(Some(&ci_token())),
    )
    .await
    .expect("admitted");
    assert!(format!("{ci:?}").starts_with("Ci("), "{ci:?}");
}
