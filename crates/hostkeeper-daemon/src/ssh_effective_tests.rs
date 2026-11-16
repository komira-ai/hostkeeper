//! The tests of `ssh_effective`: what `sshd -T` printed, read and judged for the lockout rules.

use super::*;

const STOCK: &str = "port 22\n\
    listenaddress 0.0.0.0:22\n\
    pubkeyauthentication yes\n\
    authenticationmethods any\n\
    permitrootlogin prohibit-password\n\
    passwordauthentication yes\n\
    kbdinteractiveauthentication no\n\
    usepam yes\n";

const PRINCIPALS: &str = "/etc/ssh/hostkeeper/principals.%u";

fn ours() -> Ours<'static> {
    Ours {
        ca_keys: "/etc/ssh/hostkeeper/user_ca.pub",
        principals: PRINCIPALS,
        principals_expanded: "/etc/ssh/hostkeeper/principals.root",
        revoked: "/etc/ssh/hostkeeper/revoked",
    }
}

fn with_ours(base: &str) -> Effective {
    Effective::parse(&format!(
        "{base}trustedusercakeys /etc/ssh/hostkeeper/user_ca.pub\n\
         authorizedprincipalsfile {PRINCIPALS}\n\
         revokedkeys /etc/ssh/hostkeeper/revoked\n"
    ))
}

fn groups(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// Proves a keyword's lines are kept in order, a keyword with several values keeps all of them,
/// the keyword's case is folded and a keyword with no value has no words.
#[test]
fn the_parse_keeps_each_keywords_words_in_order() {
    let text = "AllowUsers alice bob\nallowusers carol\nauthenticationmethods publickey,password keyboard-interactive\nbanner\n\n";
    let effective = Effective::parse(text);
    assert_eq!(effective.words("allowusers"), ["alice", "bob", "carol"]);
    assert_eq!(effective.first("allowusers"), Some("alice"));
    assert_eq!(
        effective.words("authenticationmethods"),
        ["publickey,password", "keyboard-interactive"]
    );
    assert!(effective.words("banner").is_empty());
    assert_eq!(effective.first("banner"), None);
    assert_eq!(effective.first("nothing"), None);
}

/// Proves the recorded auth settings hold every keyword of the rule, an unset one as empty, and
/// differ when one value differs.
#[test]
fn the_auth_settings_name_every_keyword_of_the_rule() {
    let settings = Effective::parse(STOCK).auth_settings();
    assert_eq!(settings.len(), AUTH_KEYWORDS.len());
    assert_eq!(
        settings["pubkeyauthentication"],
        vec![vec!["yes".to_owned()]]
    );
    assert!(settings["allowusers"].is_empty());
    let changed =
        Effective::parse(&STOCK.replace("passwordauthentication yes", "passwordauthentication no"));
    assert_ne!(changed.auth_settings(), settings);
}

/// Proves the address a banner is read from: the first listen address with every-address read as
/// the loopback, its own port when it has one, else the `port`, else 22, and an IPv6 address in
/// brackets or bare.
#[test]
fn the_listening_address_is_one_a_connection_reaches() {
    let at = |text: &str| Effective::parse(text).listening();
    assert_eq!(at(""), ("127.0.0.1".to_owned(), 22));
    assert_eq!(at("port 2222\n"), ("127.0.0.1".to_owned(), 2222));
    assert_eq!(at("port x\n"), ("127.0.0.1".to_owned(), 22));
    assert_eq!(
        at("port 22\nlistenaddress 0.0.0.0:2200\n"),
        ("127.0.0.1".to_owned(), 2200)
    );
    assert_eq!(
        at("port 2222\nlistenaddress 192.0.2.3\n"),
        ("192.0.2.3".to_owned(), 2222)
    );
    assert_eq!(at("listenaddress [::]:22\n"), ("::1".to_owned(), 22));
    assert_eq!(
        at("listenaddress [fe80::1]:2200\n"),
        ("fe80::1".to_owned(), 2200)
    );
    assert_eq!(at("listenaddress [fe80::1]\n"), ("fe80::1".to_owned(), 22));
    assert_eq!(at("listenaddress [fe80::1\n"), ("fe80::1".to_owned(), 22));
    assert_eq!(at("listenaddress fe80::1\n"), ("fe80::1".to_owned(), 22));
    assert_eq!(
        at("listenaddress 192.0.2.9:x\n"),
        ("192.0.2.9".to_owned(), 22)
    );
    assert_eq!(at("listenaddress :22\n"), ("127.0.0.1".to_owned(), 22));
}

/// Proves each allow and deny list keeps out the user it names, in sshd's order, and patterns
/// with `*`, `?` and a host part name users; with no list the user is in.
#[test]
fn the_allow_and_deny_lists_say_who_is_kept_out() {
    let free = Effective::parse(STOCK);
    assert_eq!(free.excluded("root", &groups(&["root"])), None);

    let deny_user = Effective::parse("denyusers ro*\nallowusers root\n");
    assert_eq!(deny_user.excluded("root", &[]), Some("denyusers"));

    let allow_user = Effective::parse("allowusers alice b?b admin@10.0.0.*\n");
    assert_eq!(allow_user.excluded("root", &[]), Some("allowusers"));
    assert_eq!(allow_user.excluded("alice", &[]), None);
    assert_eq!(allow_user.excluded("bob", &[]), None);
    assert_eq!(allow_user.excluded("admin", &[]), None);
    assert_eq!(allow_user.excluded("alicex", &[]), Some("allowusers"));

    let deny_group = Effective::parse("denygroups wheel\n");
    assert_eq!(
        deny_group.excluded("root", &groups(&["staff", "wheel"])),
        Some("denygroups")
    );
    assert_eq!(deny_group.excluded("root", &groups(&["staff"])), None);

    let allow_group = Effective::parse("allowgroups ops\n");
    assert_eq!(
        allow_group.excluded("root", &groups(&["staff"])),
        Some("allowgroups")
    );
    assert_eq!(allow_group.excluded("root", &[]), Some("allowgroups"));
    assert_eq!(
        allow_group.excluded("root", &groups(&["staff", "ops"])),
        None
    );
}

/// Proves a certificate alone logs a user in under the stock settings, and each setting that
/// would stop it is named: public keys off, a method list with no alternative of `publickey`
/// alone, and for root a `permitrootlogin` of `no` or `forced-commands-only`; an account other
/// than root is not held to `permitrootlogin`.
#[test]
fn a_certificate_login_needs_public_keys_alone_and_root_login_for_root() {
    let ok = |text: &str, user: &str| Effective::parse(text).cert_login(user, None);
    assert_eq!(ok(STOCK, "root"), Ok(()));
    assert_eq!(
        ok(&STOCK.replace("yes\nauth", "no\nauth"), "root"),
        Err(Failing("pubkeyauthentication"))
    );
    assert_eq!(
        ok("port 22\n", "root"),
        Err(Failing("pubkeyauthentication"))
    );
    let methods = |value: &str| {
        STOCK.replace(
            "authenticationmethods any",
            &format!("authenticationmethods {value}"),
        )
    };
    assert_eq!(
        ok(&methods("publickey,password"), "root"),
        Err(Failing("authenticationmethods"))
    );
    assert_eq!(ok(&methods("publickey,password publickey"), "root"), Ok(()));
    assert_eq!(
        ok(&methods("password"), "root"),
        Err(Failing("authenticationmethods"))
    );
    let root = |value: &str| STOCK.replace("prohibit-password", value);
    assert_eq!(ok(&root("yes"), "root"), Ok(()));
    assert_eq!(ok(&root("without-password"), "root"), Ok(()));
    assert_eq!(ok(&root("no"), "root"), Err(Failing("permitrootlogin")));
    assert_eq!(
        ok(&root("forced-commands-only"), "root"),
        Err(Failing("permitrootlogin"))
    );
    assert_eq!(ok(&root("no"), "deploy"), Ok(()));
}

/// Proves the trust files are ours only when each of the three keywords has exactly the one path
/// the drop-in names (the principals file with `%u` or expanded), and the keyword that is not is
/// named and read as shadowed by the stock configuration.
#[test]
fn the_trust_files_are_ours_only_when_each_keyword_carries_our_path() {
    let ours = ours();
    assert_eq!(with_ours(STOCK).cert_login("root", Some(&ours)), Ok(()));
    let expanded = Effective::parse(&format!(
        "{STOCK}trustedusercakeys {}\nauthorizedprincipalsfile {}\nrevokedkeys {}\n",
        ours.ca_keys, ours.principals_expanded, ours.revoked
    ));
    assert_eq!(expanded.cert_login("root", Some(&ours)), Ok(()));

    assert_eq!(
        Effective::parse(STOCK).cert_login("root", Some(&ours)),
        Err(Failing("trustedusercakeys"))
    );
    let other_ca = STOCK.to_owned()
        + "trustedusercakeys /etc/ssh/other.pub\nauthorizedprincipalsfile /etc/ssh/hostkeeper/principals.%u\nrevokedkeys /etc/ssh/hostkeeper/revoked\n";
    assert_eq!(
        Effective::parse(&other_ca).cert_login("root", Some(&ours)),
        Err(Failing("trustedusercakeys"))
    );
    let no_principals = STOCK.to_owned() + "trustedusercakeys /etc/ssh/hostkeeper/user_ca.pub\n";
    assert_eq!(
        Effective::parse(&no_principals).cert_login("root", Some(&ours)),
        Err(Failing("authorizedprincipalsfile"))
    );
    let no_revoked = no_principals + "authorizedprincipalsfile /etc/ssh/hostkeeper/principals.%u\n";
    assert_eq!(
        Effective::parse(&no_revoked).cert_login("root", Some(&ours)),
        Err(Failing("revokedkeys"))
    );
}

/// Proves only the three trust keywords are read as shadowed.
#[test]
fn only_the_trust_keywords_are_shadowed() {
    for keyword in [
        "trustedusercakeys",
        "authorizedprincipalsfile",
        "revokedkeys",
    ] {
        assert!(Failing(keyword).is_shadowed(), "{keyword}");
    }
    for keyword in [
        "pubkeyauthentication",
        "authenticationmethods",
        "permitrootlogin",
    ] {
        assert!(!Failing(keyword).is_shadowed(), "{keyword}");
    }
}
