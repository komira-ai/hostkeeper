use super::{Authorization, CredentialError, DnsCredential, DnsError, challenge_record};

fn parse(json: &str) -> Result<DnsCredential, CredentialError> {
    DnsCredential::parse(json.as_bytes())
}

fn reason(json: &str) -> String {
    parse(json).expect_err("refused").to_string()
}

/// Proves: both kinds of credential are read, a key and secret as `sso-key` and a token as
/// `bearer`, and each makes the header of its kind; the debug text of either holds no value.
///
/// Catches: a credential kind that cannot be stored beside the first, which a later move to the
/// provider's personal access token would need.
#[test]
fn a_credential_is_an_sso_key_or_a_bearer_token_and_prints_nothing() {
    let sso = parse(r#"{"provider":"godaddy","kind":"sso-key","key":"KEY1","secret":"SEC2"}"#)
        .expect("an sso-key");
    assert_eq!(sso.provider, "godaddy");
    assert_eq!(sso.authorization.header(), "sso-key KEY1:SEC2");
    let bearer =
        parse(r#"{"provider":"godaddy","kind":"bearer","token":"TOK3"}"#).expect("a token");
    assert_eq!(
        bearer.authorization,
        Authorization::Bearer {
            token: "TOK3".to_owned()
        }
    );
    assert_eq!(bearer.authorization.header(), "Bearer TOK3");
    let shown = format!("{sso:?} {bearer:?}");
    for value in ["KEY1", "SEC2", "TOK3"] {
        assert!(!shown.contains(value), "{shown}");
    }
    assert!(shown.contains("SsoKey") && shown.contains("Bearer"));
}

/// Proves: a record that is not a credential is refused with a reason that names the field and
/// holds none of the record's text: not JSON, an unknown field, an unknown kind, a missing or
/// empty field, a character a header cannot hold, a field over 512 bytes, and an empty provider.
///
/// Catches: an error text that echoes the secret into a log.
#[test]
fn a_credential_that_is_not_one_is_refused_without_echoing_it() {
    assert_eq!(
        reason("SECRET-TEXT"),
        "the DNS credential is not understood: it is not the JSON of a credential"
    );
    assert_eq!(
        reason(r#"{"provider":"godaddy","kind":"bearer","token":"t","extra":"SECRET"}"#),
        "the DNS credential is not understood: it is not the JSON of a credential"
    );
    assert_eq!(
        reason(r#"{"provider":"godaddy","kind":"basic","token":"SECRET"}"#),
        "the DNS credential is not understood: the kind is neither sso-key nor bearer"
    );
    assert_eq!(
        reason(r#"{"provider":"godaddy","kind":"sso-key","key":"k"}"#),
        "the DNS credential is not understood: the secret is missing"
    );
    assert_eq!(
        reason(r#"{"provider":"godaddy","kind":"bearer","token":""}"#),
        "the DNS credential is not understood: the token is empty, too long or has a bad character"
    );
    assert_eq!(
        reason(r#"{"provider":"godaddy","kind":"bearer","token":"a b"}"#),
        "the DNS credential is not understood: the token is empty, too long or has a bad character"
    );
    assert_eq!(
        reason(r#"{"provider":"godaddy","kind":"sso-key","key":"a:b","secret":"s"}"#),
        "the DNS credential is not understood: the key is empty, too long or has a bad character"
    );
    let long = "x".repeat(513);
    assert_eq!(
        reason(&format!(
            r#"{{"provider":"godaddy","kind":"bearer","token":"{long}"}}"#
        )),
        "the DNS credential is not understood: the token is empty, too long or has a bad character"
    );
    assert_eq!(
        reason(r#"{"provider":"","kind":"bearer","token":"t"}"#),
        "the DNS credential is not understood: the provider is empty"
    );
}

/// Proves: the challenge record of `mdm.internal.example.net` in the zone `example.net` is
/// `_acme-challenge.mdm.internal`, the zone itself has `_acme-challenge`, and a name that is not
/// under the zone (another zone, a longer label that ends in the zone's letters, the zone with a
/// bare dot) has none.
///
/// Catches: a zone worked out by removing a label from the name, and a suffix match that takes
/// `xexample.net` for a name in `example.net`.
#[test]
fn the_challenge_record_is_named_inside_the_zone_setting() {
    assert_eq!(
        challenge_record("mdm.internal.example.net", "example.net").as_deref(),
        Some("_acme-challenge.mdm.internal")
    );
    assert_eq!(
        challenge_record("mdm.internal.example.net", "internal.example.net").as_deref(),
        Some("_acme-challenge.mdm")
    );
    assert_eq!(
        challenge_record("example.net", "example.net").as_deref(),
        Some("_acme-challenge")
    );
    assert_eq!(challenge_record("mdm.example.org", "example.net"), None);
    assert_eq!(challenge_record("mdm.xexample.net", "example.net"), None);
    assert_eq!(challenge_record(".example.net", "example.net"), None);
}

/// Proves: the two kinds of provider error say what they are.
#[test]
fn a_dns_error_names_its_cause() {
    assert_eq!(
        DnsError::Rejected { status: 403 }.to_string(),
        "the DNS provider refused the credential (status 403)"
    );
    assert_eq!(
        DnsError::Failed("x".to_owned()).to_string(),
        "the DNS provider call failed: x"
    );
}
