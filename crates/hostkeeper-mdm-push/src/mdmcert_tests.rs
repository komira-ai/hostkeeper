use super::*;
use crate::request::csr_pem;
use crate::testkit::{FakeMdmcert, fixture_key, signed_request_plist};
use crate::tls_server_tests::{Auth, Reply, Server};

const EMAIL: &str = "ops@example.org";

#[test]
fn the_api_key_is_the_public_constant_of_the_services_client() {
    assert_eq!(API_KEY.len(), 64);
    assert!(API_KEY.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(HOST_NAME, "mdmcert.download");
    assert_eq!(SIGN_REQUEST_PATH, "/api/v1/signrequest");
}

#[test]
fn the_body_is_the_json_of_micromdms_client_with_the_pem_in_base64() {
    let body = sign_request_body(EMAIL, "CSR PEM\n", "CERT PEM\n");
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["email"], EMAIL);
    assert_eq!(json["key"], API_KEY);
    assert_eq!(
        STANDARD.decode(json["csr"].as_str().unwrap()).unwrap(),
        b"CSR PEM\n"
    );
    assert_eq!(
        STANDARD.decode(json["encrypt"].as_str().unwrap()).unwrap(),
        b"CERT PEM\n"
    );
    assert_eq!(json.as_object().unwrap().len(), 4);
}

async fn submit_to(fake: &FakeMdmcert) -> Result<(), MdmcertError> {
    submit(fake, EMAIL, "csr", "cert", LONG).await
}

#[tokio::test]
async fn a_success_result_is_taken_and_the_request_is_posted_once() {
    let fake = FakeMdmcert::new();
    submit_to(&fake).await.unwrap();
    assert_eq!(fake.posted().len(), 1);
    assert_eq!(fake.posted()[0], sign_request_body(EMAIL, "csr", "cert"));
}

#[tokio::test]
async fn every_way_the_service_can_decline_is_its_own_error() {
    let answer = |status: u16, body: &str| {
        Ok(HttpReply {
            status,
            body: body.as_bytes().to_vec(),
        })
    };
    let fake = FakeMdmcert::new();
    fake.answer_next(answer(500, ""));
    assert_eq!(submit_to(&fake).await, Err(MdmcertError::Status(500)));
    fake.answer_next(answer(200, "<html>"));
    assert_eq!(submit_to(&fake).await, Err(MdmcertError::Malformed));
    fake.answer_next(answer(
        200,
        r#"{"result":"failure","reason":"unregistered email"}"#,
    ));
    let refused = submit_to(&fake).await.unwrap_err();
    assert_eq!(
        refused,
        MdmcertError::Refused {
            result: "failure".to_owned(),
            reason: "unregistered email".to_owned()
        }
    );
    assert_eq!(
        refused.to_string(),
        "mdmcert.download refused the request: failure: unregistered email"
    );
    fake.answer_next(answer(200, "{}"));
    assert!(matches!(
        submit_to(&fake).await,
        Err(MdmcertError::Refused { .. })
    ));
    fake.answer_next(Err(TransportError("no route".to_owned())));
    assert_eq!(
        submit_to(&fake).await,
        Err(MdmcertError::Transport("no route".to_owned()))
    );
}

const LONG: Duration = Duration::from_secs(60);

fn transport(server: &Server) -> HttpsTransport {
    let roots = tls::roots_of(std::slice::from_ref(&server.ca)).unwrap();
    let endpoint = Endpoint::new("localhost", 443).dialing(&server.addr.to_string());
    HttpsTransport::new(endpoint, roots)
}

#[tokio::test]
async fn the_https_transport_posts_json_with_a_content_type_to_the_sign_request_path() {
    let server = Server::start(false, Auth::None).await;
    server.answer_next(Reply::status(200, r#"{"result":"success"}"#));
    let transport = transport(&server);
    submit(&transport, EMAIL, "csr", "cert", LONG)
        .await
        .unwrap();
    let seen = server.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].path, "/api/v1/signrequest");
    assert_eq!(seen[0].header("content-type"), Some("application/json"));
    assert_eq!(seen[0].header("user-agent"), Some("hostkeeper-coordinator"));
    assert_eq!(seen[0].header("host"), Some("localhost"));
    assert_eq!(seen[0].body, sign_request_body(EMAIL, "csr", "cert"));
}

#[tokio::test]
async fn the_https_transport_reports_a_refusal_status_and_a_service_that_is_not_there() {
    let server = Server::start(false, Auth::None).await;
    server.answer_next(Reply::status(400, r#"{"result":"failure"}"#));
    let transport_up = transport(&server);
    assert_eq!(
        submit(&transport_up, EMAIL, "csr", "cert", LONG).await,
        Err(MdmcertError::Status(400))
    );
    let dead = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let roots = tls::roots_of(std::slice::from_ref(&server.ca)).unwrap();
    let endpoint = Endpoint::new("localhost", 443).dialing(&dead.to_string());
    let gone = HttpsTransport::new(endpoint, roots);
    assert!(matches!(
        submit(&gone, EMAIL, "csr", "cert", LONG).await,
        Err(MdmcertError::Transport(_))
    ));
}

/// The service never answers, so the only way out is the timeout, on a virtual clock.
#[tokio::test(start_paused = true)]
async fn a_service_that_never_answers_is_a_transport_error() {
    let server = Server::start(false, Auth::None).await;
    let mut never = Reply::ok();
    never.hang = true;
    server.answer_next(never);
    let transport = transport(&server);
    assert!(matches!(
        submit(&transport, EMAIL, "csr", "cert", LONG).await,
        Err(MdmcertError::Transport(_))
    ));
}

#[test]
fn the_production_transport_is_made_for_the_real_service() {
    let transport = HttpsTransport::production();
    assert_eq!(transport.endpoint, Endpoint::new("mdmcert.download", 443));
    assert!(!transport.roots.roots.is_empty());
}

#[test]
fn a_signed_request_for_the_pending_csr_is_accepted_in_each_form_the_csr_may_take() {
    let csr = csr_pem(fixture_key(0), EMAIL).unwrap();
    check_signed_request(&signed_request_plist(&csr), &csr).unwrap();
    let der = {
        use rustls::pki_types::CertificateSigningRequestDer;
        use rustls::pki_types::pem::PemObject as _;
        CertificateSigningRequestDer::from_pem_slice(csr.as_bytes())
            .unwrap()
            .as_ref()
            .to_vec()
    };
    let with_csr = |value: &str| {
        let mut dict = plist::Dictionary::new();
        dict.insert(
            "PushCertRequestCSR".to_owned(),
            plist::Value::String(value.to_owned()),
        );
        dict.insert(
            "PushCertCertificateChain".to_owned(),
            plist::Value::String("c".to_owned()),
        );
        dict.insert(
            "PushCertSignature".to_owned(),
            plist::Value::String("s".to_owned()),
        );
        let mut out = Vec::new();
        plist::Value::Dictionary(dict)
            .to_writer_binary(&mut out)
            .unwrap();
        out
    };
    // The PEM as it is, base64 of the DER, base64 of the PEM.
    check_signed_request(&with_csr(&csr), &csr).unwrap();
    check_signed_request(&with_csr(&STANDARD.encode(&der)), &csr).unwrap();
    check_signed_request(&with_csr(&STANDARD.encode(csr.as_bytes())), &csr).unwrap();
}

#[test]
fn a_reply_that_is_not_a_signed_request_for_this_csr_is_refused() {
    let csr = csr_pem(fixture_key(0), EMAIL).unwrap();
    let other = csr_pem(fixture_key(1), EMAIL).unwrap();
    assert_eq!(
        check_signed_request(&signed_request_plist(&other), &csr),
        Err(SignedRequestError::CsrMismatch)
    );
    assert_eq!(
        check_signed_request(b"plain text", &csr),
        Err(SignedRequestError::NotPlist)
    );
    let mut out = Vec::new();
    plist::Value::Array(vec![]).to_writer_xml(&mut out).unwrap();
    assert_eq!(
        check_signed_request(&out, &csr),
        Err(SignedRequestError::NotPlist)
    );
    let partial = |without: &str| {
        let mut dict = plist::Dictionary::new();
        for key in [
            "PushCertRequestCSR",
            "PushCertCertificateChain",
            "PushCertSignature",
        ] {
            if key != without {
                dict.insert(key.to_owned(), plist::Value::String("x".to_owned()));
            }
        }
        let mut out = Vec::new();
        plist::Value::Dictionary(dict)
            .to_writer_xml(&mut out)
            .unwrap();
        out
    };
    for key in [
        "PushCertRequestCSR",
        "PushCertCertificateChain",
        "PushCertSignature",
    ] {
        assert_eq!(
            check_signed_request(&partial(key), &csr),
            Err(SignedRequestError::Missing(match key {
                "PushCertRequestCSR" => "PushCertRequestCSR",
                "PushCertCertificateChain" => "PushCertCertificateChain",
                _ => "PushCertSignature",
            }))
        );
    }
    // A pending CSR that is not a CSR cannot be matched.
    assert_eq!(
        check_signed_request(&signed_request_plist(&csr), "not pem"),
        Err(SignedRequestError::CsrMismatch)
    );
    assert_eq!(
        SignedRequestError::Missing("PushCertSignature").to_string(),
        "the signed request has no PushCertSignature"
    );
}
