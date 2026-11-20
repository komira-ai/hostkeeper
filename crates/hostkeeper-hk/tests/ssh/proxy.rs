//! `hk ssh-proxy`: the byte stream between ssh and the coordinator's tunnel.

use hostkeeper_hk::ssh::Verb;

use crate::fake::{
    Closed, Double, HOST, NOW, Rig, TOKEN, Tunnel, certificate_reply, invoke, invoke_into,
};

fn proxy(rig: &Rig, double: &Double, now: u64, input: &[u8]) -> crate::fake::Outcome {
    invoke(rig, Verb::Proxy, &[HOST], double, now, input.to_vec())
}

fn queue(rig: &Rig, modes: &[Tunnel]) {
    rig.script
        .tunnels
        .lock()
        .expect("tunnels")
        .extend(modes.iter().cloned());
}

/// Proves: stdin reaches the host, the host's bytes reach stdout, end of stdin is a half-close
/// after which the host's last reply still arrives, the first frame names the host and the
/// serial of the certificate, and every call carries the token as a bearer.
#[test]
fn a_proxy_carries_bytes_both_ways_and_a_half_close_keeps_the_reply() {
    let rig = Rig::start();
    let outcome = proxy(&rig, &Double::new(), NOW, b"SSH-2.0-client\r\n");
    assert_eq!(outcome.ok(), 0);
    assert_eq!(outcome.stdout, b"SSH-2.0-client\r\nbye");
    let (opens, auths) = rig.seen(|s| (s.opens.clone(), s.authorizations.clone()));
    assert_eq!(opens.len(), 1);
    assert_eq!(
        (opens[0].host_id.as_str(), opens[0].cert_serial),
        (HOST, 41)
    );
    assert!(
        auths.iter().all(|a| a == &format!("Bearer {TOKEN}")),
        "{auths:?}"
    );
    assert_eq!(auths.len(), 2, "one signing and one tunnel");
}

/// Proves: no data frame is longer than 16 KiB, and a long input arrives whole and in order.
#[test]
fn a_long_input_is_cut_into_frames_of_at_most_16_kib_and_arrives_whole() {
    let rig = Rig::start();
    let input: Vec<u8> = (0..50_000_u32)
        .map(|i| u8::try_from(i % 251).expect("a byte"))
        .collect();
    let outcome = proxy(&rig, &Double::new(), NOW, &input);
    assert_eq!(outcome.ok(), 0);
    let mut expected = input.clone();
    expected.extend_from_slice(b"bye");
    assert_eq!(outcome.stdout, expected);
    let frames = rig.seen(|s| s.frames.clone());
    assert!(frames.iter().all(|f| !f.is_empty() && f.len() <= 16 * 1024));
    assert_eq!(frames.concat(), input);
}

/// Proves: an instance that is stopping (`draining`) is asked again once and the second answer
/// is used; two refusals in a row, and any other refusal, are the end, with no further try.
#[test]
fn draining_is_retried_once_and_nothing_else_is() {
    let rig = Rig::start();
    queue(&rig, &[Tunnel::Refuse("draining"), Tunnel::Echo]);
    let outcome = proxy(&rig, &Double::new(), NOW, b"x");
    assert_eq!(outcome.ok(), 0);
    assert_eq!(rig.seen(|s| s.opens.len()), 2);

    let rig = Rig::start();
    queue(
        &rig,
        &[
            Tunnel::Refuse("draining"),
            Tunnel::Refuse("draining"),
            Tunnel::Echo,
        ],
    );
    assert_eq!(
        proxy(&rig, &Double::new(), NOW, b"x").error(),
        "refused: draining"
    );
    assert_eq!(rig.seen(|s| s.opens.len()), 2);

    let rig = Rig::start();
    queue(&rig, &[Tunnel::Refuse("bad_serial"), Tunnel::Echo]);
    assert_eq!(
        proxy(&rig, &Double::new(), NOW, b"x").error(),
        "refused: bad_serial"
    );
    assert_eq!(rig.seen(|s| s.opens.len()), 1);
}

/// Proves: how the stream ends decides the outcome. The host closing, or the stream ending, is a
/// clean end; the coordinator closing it for another cause is an error naming the cause; a
/// failure of the stream is the error it says; a first answer that is not `opened`, or none, is
/// an error.
#[test]
fn how_the_stream_ends_decides_the_outcome() {
    let cases: [(Tunnel, Result<(), &str>); 6] = [
        (Tunnel::CloseAfterOpen("host_closed"), Ok(())),
        (Tunnel::CloseAfterOpen("eof"), Ok(())),
        (Tunnel::EndAfterOpen, Ok(())),
        (
            Tunnel::CloseAfterOpen("revoked"),
            Err("the tunnel was closed: revoked"),
        ),
        (
            Tunnel::FailAfterOpen("token_revoked"),
            Err("refused: token_revoked"),
        ),
        (
            Tunnel::NotOpened,
            Err("the coordinator's first answer is not `opened`"),
        ),
    ];
    for (mode, expected) in cases {
        let rig = Rig::start();
        queue(&rig, std::slice::from_ref(&mode));
        let outcome = proxy(&rig, &Double::new(), NOW, b"");
        match expected {
            Ok(()) => assert_eq!(outcome.ok(), 0, "{mode:?}"),
            Err(text) => assert_eq!(outcome.error(), text, "{mode:?}"),
        }
    }
    let rig = Rig::start();
    queue(&rig, &[Tunnel::EmptyAnswer]);
    assert_eq!(
        proxy(&rig, &Double::new(), NOW, b"").error(),
        "the coordinator's first answer is not `opened`"
    );
}

/// Proves: a refusal of the tunnel that is not a token (a failure with a sentence) is reported as
/// no answer, not as a refusal.
#[test]
fn a_status_that_is_not_a_token_is_no_answer() {
    let rig = Rig::start();
    queue(&rig, &[Tunnel::Refuse("the instance is down")]);
    let outcome = proxy(&rig, &Double::new(), NOW, b"");
    assert_eq!(
        outcome.error(),
        "no answer from the coordinator: Unavailable: the instance is down"
    );
    assert_eq!(outcome.code, 1);
}

/// Proves: the certificate is made once and kept: a second proxy for the host signs nothing; one
/// whose certificate has under two minutes left, or whose key is another, signs again, and the
/// serial of the newest is the one opened.
#[test]
fn a_certificate_is_kept_until_it_is_near_expiry_or_the_key_changes() {
    let rig = Rig::start();
    let double = Double::new();
    proxy(&rig, &double, NOW, b"").ok();
    proxy(&rig, &double, NOW + 3000, b"").ok();
    assert_eq!(rig.seen(|s| s.signs.len()), 1);

    std::fs::remove_file(
        rig.state_dir()
            .join("hosts")
            .join(format!("{HOST}-cert.pub")),
    )
    .expect("the certificate file");
    proxy(&rig, &double, NOW, b"").ok();
    assert_eq!(
        rig.seen(|s| s.signs.len()),
        2,
        "a held record without its file is not enough"
    );

    *rig.script.sign.lock().expect("sign") = Ok(certificate_reply(42, NOW + 10_000));
    proxy(&rig, &double, NOW + 3600 - 60, b"").ok();
    assert_eq!(rig.seen(|s| s.signs.len()), 3);
    assert_eq!(
        rig.seen(|s| s.opens.last().map(|o| o.cert_serial)),
        Some(42)
    );

    let other = Double {
        agent: Some("ssh-ed25519 AAAAagentkey".into()),
        ..Double::new()
    };
    proxy(&rig, &other, NOW + 3600 - 60, b"").ok();
    assert_eq!(rig.seen(|s| s.signs.len()), 4);
    assert_eq!(
        rig.seen(|s| s.signs[3].public_key.clone()),
        "ssh-ed25519 AAAAagentkey"
    );
    assert_eq!(rig.seen(|s| s.signs[0].reason.clone()), "hk ssh-proxy");
}

/// Proves: a coordinator that answers a certificate that is not one, or whose key id has no
/// serial, gets nothing kept and nothing opened.
#[test]
fn an_answer_that_is_not_a_certificate_is_refused() {
    let rig = Rig::start();
    let mut reply = certificate_reply(1, NOW + 3600);
    reply.certificate = "ssh-ed25519 AAAA".into();
    *rig.script.sign.lock().expect("sign") = Ok(reply);
    assert_eq!(
        proxy(&rig, &Double::new(), NOW, b"").error(),
        "the coordinator's answer is not an OpenSSH certificate"
    );

    let mut reply = certificate_reply(1, NOW + 3600);
    reply.key_id = "alice".into();
    *rig.script.sign.lock().expect("sign") = Ok(reply);
    assert_eq!(
        proxy(&rig, &Double::new(), NOW, b"").error(),
        "the certificate's key id has no serial"
    );
    assert_eq!(rig.seen(|s| s.opens.len()), 0);
}

/// Proves: the coordinator's refusal of the signing is the error, and the tunnel is not tried.
#[test]
fn a_refused_signing_ends_the_proxy() {
    let rig = Rig::start();
    *rig.script.sign.lock().expect("sign") = Err(tonic::Status::permission_denied("not_permitted"));
    assert_eq!(
        proxy(&rig, &Double::new(), NOW, b"").error(),
        "refused: not_permitted"
    );
    assert_eq!(rig.seen(|s| s.opens.len()), 0);
}

/// Proves: when ssh has gone away (its end of the pipe is closed), the proxy ends cleanly at the
/// first byte it cannot hand on, instead of reading on.
#[test]
fn a_proxy_ends_when_its_output_is_closed() {
    let rig = Rig::start();
    let (code, diag) = invoke_into(
        &rig,
        Verb::Proxy,
        &[HOST],
        &Double::new(),
        NOW,
        b"hello".to_vec(),
        Closed,
    );
    assert_eq!((code, diag.as_str()), (0, ""));
}

/// Proves: a stream that fails before the coordinator says `opened` is that failure, as its
/// refusal when it is a token and as no answer when it is not (or says nothing).
#[test]
fn a_stream_that_fails_before_opened_is_that_failure() {
    let cases = [
        ("token_revoked", "refused: token_revoked"),
        ("", "no answer from the coordinator: Unavailable: "),
    ];
    for (message, expected) in cases {
        let rig = Rig::start();
        queue(&rig, &[Tunnel::FailBeforeOpened(message)]);
        assert_eq!(proxy(&rig, &Double::new(), NOW, b"").error(), expected);
    }
}
