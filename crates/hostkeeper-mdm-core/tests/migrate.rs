//! The settings of `hostkeeper-mdm migrate`: the old kit's file read, the new kit's written.

mod common;

use common::{bind, lan, site_addresses};
use hostkeeper_mdm_core::config::{ConfigError, KEYS};
use hostkeeper_mdm_core::migrate::{
    CARRIED, DROPPED, ERASE_NAMESPACE, MigrateError, Old, RENAMED, Target, new_settings,
};
use hostkeeper_mdm_core::settings::{Settings, SettingsError};

/// A settings file of kbf-deploy's `mdm` module, as a stack installed under the jobscheduler
/// kit's paths has it: no unit prefix (kbf-deploy's units are `kbf-mdm-*`), kbf-deploy's
/// `MDM_ERASE_NAMESPACE`, and ACME settings without the resolvers and the wait.
const KBF_DEPLOY_FILE: &str = "MDM_DNS_NAME=mdm.example.com
MDM_BIND=@TEST_BIND@
MDM_LAN_CIDR=@TEST_LAN@
MDM_USER=js-mdm
MDM_STATE=/var/lib/js-mdm
MDM_ETC=/etc/js-mdm
MDM_BIN=/opt/js-mdm/bin
MDM_ERASE_STATE=/var/lib/js-mdm-erase
MDM_NANOHUB_PORT=9004
MDM_SCEP_PORT=9005
MDM_DEP_PORT=9001
MDM_MEMORY_MAX=1G
MDM_CPU_QUOTA=100%
MDM_TASKS_MAX=256
MDM_DEP_NAME=js-mdm
MDM_ORG=Example
MDM_PROFILE_ID=com.example.jobscheduler.mdm
MDM_ACME_EMAIL=admin@example.com
MDM_ACME_SERVER=https://acme-v02.api.letsencrypt.org/directory
MDM_ACME_DNS_PROVIDER=godaddy
MDM_ERASE_NAMESPACE=js-mdm-erase
MDM_ERASE_PRINCIPAL=admin@example.com
MDM_ERASE_MAX_AGE=900
";

fn kbf_deploy_file() -> String {
    site_addresses(KBF_DEPLOY_FILE)
}

fn target() -> Target {
    Target {
        profile_id: "com.komira.hostkeeper.mdm".to_owned(),
        scep_ca_cn: None,
        acme_resolvers: Some("192.0.2.53:53 198.51.100.53:53".to_owned()),
        acme_propagation_wait: Some("300s".to_owned()),
    }
}

/// Catches a move that reads the old file with the new kit's rules (it would refuse the live
/// stack's `MDM_ERASE_NAMESPACE`), takes the wrong prefix for the units to stop, or loses the
/// DEP name NanoDEP keeps the ABM token under: the kbf-deploy file is read, its prefix is
/// kbf-deploy's `kbf-mdm`, and the user, the state, the secrets directory and the DEP name are
/// the old file's.
#[test]
fn a_kbf_deploy_file_is_read_with_its_own_keys_and_prefix() {
    let old = Old::parse(&kbf_deploy_file()).expect("the live layout's file is read");
    assert_eq!(old.user(), "js-mdm");
    assert_eq!(old.state().as_str(), "/var/lib/js-mdm");
    assert_eq!(old.etc().as_str(), "/etc/js-mdm");
    assert_eq!(old.dep_name(), "js-mdm");
    assert_eq!(old.unit_prefix(), "kbf-mdm");
    assert_eq!(old.get(ERASE_NAMESPACE), Some("js-mdm-erase"));
    assert_eq!(old.get("MDM_SLICE"), None);
}

/// Catches new settings that keep an old name (a user, a directory, a unit prefix or a profile
/// identifier of the old kit), lose a value that names the site or the DEP token, carry the
/// erase path's keys, or are not exactly the text the kit writes: the whole file is compared,
/// and it parses to the new names.
#[test]
fn the_new_settings_keep_the_site_and_take_the_new_names() {
    let old = Old::parse(&kbf_deploy_file()).expect("read");
    let m = new_settings(&old, &target()).expect("the new settings are accepted");
    let want = format!(
        "# hostkeeper-mdm: the MDM kit's settings, written by `hostkeeper-mdm migrate` from the
# old kit's. config.example in hostkeeper-mdm-core documents every key.
MDM_USER=hostkeeper-mdm
MDM_STATE=/var/lib/hostkeeper-mdm
MDM_ETC=/etc/hostkeeper-mdm
MDM_BIN=/opt/hostkeeper-mdm/bin
MDM_PROFILE_ID=com.komira.hostkeeper.mdm
MDM_DNS_NAME=mdm.example.com
MDM_BIND={}
MDM_LAN_CIDR={}
MDM_NANOHUB_PORT=9004
MDM_SCEP_PORT=9005
MDM_DEP_PORT=9001
MDM_MEMORY_MAX=1G
MDM_CPU_QUOTA=100%
MDM_TASKS_MAX=256
MDM_DEP_NAME=js-mdm
MDM_ORG=Example
MDM_ACME_EMAIL=admin@example.com
MDM_ACME_SERVER=https://acme-v02.api.letsencrypt.org/directory
MDM_ACME_DNS_PROVIDER=godaddy
MDM_ACME_RESOLVERS=\"192.0.2.53:53 198.51.100.53:53\"
MDM_ACME_PROPAGATION_WAIT=300s
",
        bind(),
        lan()
    );
    assert_eq!(m.text, want);
    let c = &m.config;
    assert_eq!(c.user(), "hostkeeper-mdm");
    assert_eq!(c.state().as_str(), "/var/lib/hostkeeper-mdm");
    assert_eq!(c.unit_prefix(), "hostkeeper-mdm");
    assert_eq!(c.slice(), "hostkeeper-mdm.slice");
    assert_eq!(c.pkg_dir().as_str(), "/var/lib/hostkeeper-mdm-pkg");
    assert_eq!(c.profile_id(), "com.komira.hostkeeper.mdm");
    assert_eq!(c.dep_name(), "js-mdm");
    assert_eq!(c.scep_ca().common_name(), "Example MDM SCEP CA");
    assert_eq!(c.ade().org_magic(), "hostkeeper-mdm");
    assert_eq!(c.signature_namespace(), "hostkeeper-mdm-erase");
    let acme = m.acme.expect("ACME settings");
    assert_eq!(acme.resolvers_arg(), "192.0.2.53:53,198.51.100.53:53");
    assert_eq!(acme.propagation_wait_arg(), "300s");
}

/// Catches a file of kbf's or jobscheduler's `mdm-core` read as if it were kbf-deploy's: its
/// own unit prefix is the one whose units stop, its old names (slice, CA, `org_magic`, DEP
/// certificate, namespace) do not reach the new file, and its texts and numbers that do not
/// name the old kit are kept; a given SCEP CA name is the new CA's.
#[test]
fn a_core_file_keeps_its_texts_and_loses_its_old_names() {
    let text = format!(
        "{}MDM_UNIT_PREFIX=js-mdm
MDM_SLICE=js-mdm.slice
MDM_PROFILE_DISPLAY_NAME=\"Example farm MDM\"
MDM_ADE_PROFILE_NAME=Example farm
MDM_ADE_ORG_MAGIC=js-mdm
MDM_ADE_SKIP_ITEMS=\"Siri TOS\"
MDM_LANGUAGE=de
MDM_REGION=DE
MDM_SCEP_CA_CN=js-mdm SCEP CA
MDM_SCEP_CA_OU=js-mdm
MDM_SCEP_CA_YEARS=5
MDM_DEP_CERT_CN=js-mdm
MDM_DEP_CERT_DAYS=30
MDM_SIGNATURE_NAMESPACE=js-mdm-erase
MDM_ACME_RESOLVERS=\"192.0.2.1:53\"
MDM_ACME_PROPAGATION_WAIT=120s
",
        kbf_deploy_file().replace("MDM_ERASE_NAMESPACE=js-mdm-erase\n", "")
    );
    let old = Old::parse(&text).expect("read");
    assert_eq!(old.unit_prefix(), "js-mdm");
    let given = Target {
        profile_id: "com.example.hostkeeper.mdm".to_owned(),
        scep_ca_cn: Some("Example Farm MDM CA".to_owned()),
        acme_resolvers: None,
        acme_propagation_wait: None,
    };
    let m = new_settings(&old, &given).expect("accepted");
    for gone in [
        "js-mdm.slice",
        "js-mdm SCEP CA",
        "MDM_ADE_ORG_MAGIC",
        "MDM_SLICE",
    ] {
        assert!(!m.text.contains(gone), "{gone}");
    }
    let c = &m.config;
    assert_eq!(c.profile_display_name(), "Example farm MDM");
    assert_eq!(c.ade().profile_name(), "Example farm");
    assert_eq!(c.ade().skip_items(), ["Siri", "TOS"]);
    assert_eq!((c.ade().language(), c.ade().region()), ("de", "DE"));
    assert_eq!(c.scep_ca().common_name(), "Example Farm MDM CA");
    assert_eq!(c.scep_ca().organizational_unit(), "hostkeeper-mdm");
    assert_eq!(c.scep_ca().years(), 5);
    assert_eq!(c.dep_cert().common_name(), "hostkeeper-mdm");
    assert_eq!(c.dep_cert().days(), 30);
    let acme = m.acme.expect("ACME");
    assert_eq!(acme.resolvers_arg(), "192.0.2.1:53");
    assert_eq!(acme.propagation_wait_arg(), "120s");
}

/// Catches a key of the old kits that the move neither writes nor names as renamed or dropped,
/// so a setting would vanish unseen: every key the old files may set is in exactly one of the
/// three lists, and the lists name no other key.
#[test]
fn every_old_key_is_carried_renamed_or_dropped_exactly_once() {
    let mut old_keys: Vec<&str> = KEYS.to_vec();
    old_keys.push(ERASE_NAMESPACE);
    for key in &old_keys {
        let n = [&CARRIED[..], &RENAMED[..], &DROPPED[..]]
            .iter()
            .filter(|list| list.contains(key))
            .count();
        assert_eq!(n, 1, "{key}");
    }
    assert_eq!(
        CARRIED.len() + RENAMED.len() + DROPPED.len(),
        old_keys.len()
    );
}

/// Catches a value written so that the settings parser reads it back differently: a value
/// with spaces, and one that starts or ends with a quote, come back exactly.
#[test]
fn a_written_value_reads_back_exactly() {
    for org in ["Example Org", "\"Quoted", "Tail\"", "\"Both\""] {
        let text = kbf_deploy_file().replace("MDM_ORG=Example\n", &format!("MDM_ORG=\"{org}\"\n"));
        let old = Old::parse(&text).expect("read");
        assert_eq!(old.get("MDM_ORG"), Some(org));
        let m = new_settings(&old, &target()).expect("accepted");
        let keys: Vec<&str> = KEYS.to_vec();
        let back = Settings::parse(&m.text, &keys).expect("parses");
        assert_eq!(back.get("MDM_ORG"), Some(org), "{}", m.text);
        assert_eq!(m.config.org(), org);
    }
}

/// Catches an old file the move would act on without the values it reads: an unknown key,
/// a missing or malformed user, state, secrets directory or DEP name, and a malformed prefix
/// are refused, each by its key.
#[test]
fn an_old_file_without_what_the_move_reads_is_refused() {
    let file = kbf_deploy_file();
    let unknown = format!("{file}MDM_SOMETHING=1\n");
    assert_eq!(
        Old::parse(&unknown).unwrap_err(),
        MigrateError::Old(ConfigError::Settings(SettingsError::Unknown {
            line: 24,
            key: "MDM_SOMETHING".to_owned(),
        }))
    );
    let cases = [
        (
            "MDM_USER=js-mdm\n",
            "",
            "the old settings: MDM_USER is not set (settings file)",
        ),
        (
            "MDM_USER=js-mdm\n",
            "MDM_USER=Js Mdm\n",
            "the old settings: MDM_USER=Js Mdm: not a user name",
        ),
        (
            "MDM_STATE=/var/lib/js-mdm\n",
            "",
            "the old settings: MDM_STATE is not set (settings file)",
        ),
        (
            "MDM_STATE=/var/lib/js-mdm\n",
            "MDM_STATE=var/lib\n",
            "the old settings: MDM_STATE=var/lib: not an absolute path with no empty, . or .. part",
        ),
        (
            "MDM_ETC=/etc/js-mdm\n",
            "",
            "the old settings: MDM_ETC is not set (settings file)",
        ),
        (
            "MDM_DEP_NAME=js-mdm\n",
            "",
            "the old settings: MDM_DEP_NAME is not set (settings file)",
        ),
        (
            "MDM_DEP_NAME=js-mdm\n",
            "MDM_DEP_NAME=-x\n",
            "the old settings: MDM_DEP_NAME=-x: not a name of letters, digits, . _ - that starts with a letter or digit",
        ),
        (
            "MDM_DNS_NAME=",
            "MDM_UNIT_PREFIX=Bad\nMDM_DNS_NAME=",
            "the old settings: MDM_UNIT_PREFIX=Bad: not lower-case letters, digits and - from a letter",
        ),
    ];
    for (from, to, want) in cases {
        let text = file.replacen(from, to, 1);
        assert_ne!(text, file);
        assert_eq!(Old::parse(&text).unwrap_err().to_string(), want);
    }
}

/// Catches new settings written although the new kit would refuse them: a malformed profile
/// identifier is refused by the new kit's parser, and ACME settings that lack the resolvers
/// (a kbf-deploy file without the flags) are refused by name, with the flags that give them;
/// a file with no ACME key and no flag has no ACME settings.
#[test]
fn new_settings_the_new_kit_would_refuse_are_never_written() {
    let old = Old::parse(&kbf_deploy_file()).expect("read");
    let bad_id = Target {
        profile_id: "Not An Id".to_owned(),
        ..target()
    };
    let err = new_settings(&old, &bad_id).unwrap_err();
    assert!(matches!(err, MigrateError::New(_)), "{err:?}");
    assert!(
        err.to_string()
            .starts_with("the new settings: MDM_PROFILE_ID=Not An Id: "),
        "{err}"
    );

    let no_flags = Target {
        acme_resolvers: None,
        acme_propagation_wait: None,
        ..target()
    };
    assert_eq!(
        new_settings(&old, &no_flags).unwrap_err().to_string(),
        "the new settings: MDM_ACME_RESOLVERS is not set (settings file) (the old file sets ACME keys; --acme-resolvers and --acme-propagation-wait give the two a kbf-deploy file may lack)"
    );

    let no_acme: String = kbf_deploy_file()
        .lines()
        .filter(|l| !l.starts_with("MDM_ACME_"))
        .map(|l| format!("{l}\n"))
        .collect();
    let old = Old::parse(&no_acme).expect("read");
    let m = new_settings(&old, &no_flags).expect("accepted");
    assert_eq!(m.acme, None);
    assert!(!m.text.contains("MDM_ACME_"));
}
