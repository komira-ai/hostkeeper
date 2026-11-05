//! The declaration that is left (B26 deletes it): its fields, its tables, and the rules on the
//! values that reached a unit property.

mod support;

use std::collections::BTreeSet;

use hostkeeper_hook::Step;
use hostkeeper_rights::declaration::MAX_SECS;
use hostkeeper_rights::load::Violation;
use hostkeeper_rights::names::{Capability, Family, PathError};
use hostkeeper_rights::{Declaration, Refusal};
use support::{SAMPLE, refusal, sample, with, with_in};

fn caps(names: &[&str]) -> BTreeSet<Capability> {
    names
        .iter()
        .map(|name| Capability::try_from((*name).to_owned()).unwrap())
        .collect()
}

fn field_of(refusal: &Refusal) -> String {
    match refusal {
        Refusal::Value { field, .. } => field.clone(),
        Refusal::Path { field, .. } => (*field).to_owned(),
        other => panic!("not a field refusal: {other:?}"),
    }
}

/// Catches a field of the declaration that is left being dropped or renamed.
#[test]
fn the_example_reads_into_every_field() {
    let d = sample();
    assert_eq!(d.name, "tierstore");
    assert_eq!(d.binary, "tierstore");
    assert_eq!(d.hook_args, ["update-hook"]);
    assert_eq!(d.state_dir, "/var/lib/updater/hooks/tierstore");
    assert_eq!(d.hook.user, "tierstore-update");
    assert_eq!(d.hook.group, "tierstore-update");
    assert!(d.hook.capabilities.is_empty());
    assert_eq!(d.hook.read_paths.len(), 4);
    assert!(d.hook.write_paths.is_empty());
    assert_eq!(d.hook.address_families.len(), 4);
    assert!(
        d.hook
            .address_families
            .contains(&Family::try_from("AF_NETLINK".to_owned()).unwrap())
    );
    assert!(d.hook.environment.is_empty());
    assert_eq!(d.hook.timeout_secs.get(Step::Prepare), 600);
    assert_eq!(d.hook.timeout_secs.get(Step::Cutover), 300);
    assert_eq!(d.hook.timeout_secs.get(Step::Verify), 1000);
    assert_eq!(d.hook.timeout_secs.get(Step::Rollback), 600);
    assert_eq!(d.hook.timeout_secs.get(Step::Cleanup), 1300);
}

/// Catches a declaration that grew an `uninstall` step: it has neither a bound nor a capability
/// entry for it, so the launcher of declarations never runs it.
#[test]
fn a_declaration_has_no_uninstall_step() {
    let d = sample();
    assert_eq!(d.hook.timeout_secs.get(Step::Uninstall), 0);
    assert_eq!(d.hook.step_capabilities.get(Step::Uninstall), None);
}

/// Catches a default that is not the old one: `hook_args` is `["update-hook"]`, and every set
/// starts empty.
#[test]
fn a_minimal_declaration_takes_the_defaults() {
    let text = r#"
name = "svc"
binary = "bin/svc"
state_dir = "/var/lib/updater/hooks/svc"
[hook]
user = "svc-update"
group = "svc-update"
[hook.timeout_secs]
prepare = 1
cutover = 1
verify = 10
rollback = 10
cleanup = 1
"#;
    let d = Declaration::parse(text).unwrap();
    assert_eq!(d.hook_args, ["update-hook"]);
    assert!(d.hook.capabilities.is_empty());
    assert!(d.hook.read_paths.is_empty());
    assert!(d.hook.environment.is_empty());
    for step in Step::ALL {
        assert_eq!(d.hook.step_capabilities.get(step), None);
    }
}

/// Catches `hook_args = []` being replaced by the default: an executable that takes the step
/// first is declared with an empty list.
#[test]
fn an_empty_hook_args_stays_empty() {
    let d = Declaration::parse(&with(
        "name = \"tierstore\"",
        "name = \"tierstore\"\nhook_args = []",
    ))
    .unwrap();
    assert!(d.hook_args.is_empty());
}

/// Catches a step falling back wrongly: a step with no entry holds the hook-wide grant; a step
/// with an empty entry holds nothing.
#[test]
fn a_step_holds_its_own_capabilities_or_the_hook_wide_grant() {
    let wide = with("capabilities = []", "capabilities = [\"CAP_KILL\"]");
    let d = Declaration::parse(&wide).unwrap();
    assert_eq!(
        d.hook.capabilities_for(Step::Prepare),
        &caps(&["CAP_NET_ADMIN"])
    );
    assert!(d.hook.capabilities_for(Step::Verify).is_empty());
    let none = with_in(&wide, "verify = []\n", "");
    let d = Declaration::parse(&none).unwrap();
    assert_eq!(d.hook.capabilities_for(Step::Verify), &caps(&["CAP_KILL"]));
    assert_eq!(d.hook.capabilities_for(Step::Cleanup), &caps(&[]));
}

/// Catches a key that is accepted and ignored, and a key of the deleted declaration format being
/// read: an unknown key in any table refuses the file.
#[test]
fn an_unknown_key_in_any_table_is_refused() {
    for (anchor, extra) in [
        ("name = \"tierstore\"", "name = \"tierstore\"\nsurprise = 1"),
        (
            "name = \"tierstore\"",
            "name = \"tierstore\"\nuid = \"tierstore\"",
        ),
        (
            "name = \"tierstore\"",
            "name = \"tierstore\"\nverify_deadline_secs = 1",
        ),
        ("[hook]\n", "[hook]\nsurprise = 1\n"),
        ("[hook]\n", "[hook]\nunits = []\n"),
        (
            "[hook.step_capabilities]\n",
            "[hook.step_capabilities]\nsurprise = []\n",
        ),
        (
            "[hook.timeout_secs]\n",
            "[hook.timeout_secs]\nsurprise = 1\n",
        ),
    ] {
        assert!(
            matches!(refusal(&with(anchor, extra)), Refusal::Syntax(_)),
            "{extra}"
        );
    }
}

/// Catches text that is not a declaration at all, and a required key left out.
#[test]
fn syntax_and_missing_keys_are_refused() {
    assert!(matches!(refusal("not toml ["), Refusal::Syntax(_)));
    assert!(matches!(refusal(""), Refusal::Syntax(_)));
    for missing in [
        "binary = \"tierstore\"\n",
        "verify = 1000\n",
        "prepare = 600\n",
    ] {
        assert!(
            matches!(refusal(&with(missing, "")), Refusal::Syntax(_)),
            "{missing}"
        );
    }
    assert!(matches!(
        refusal(&with(
            "\"CAP_NET_ADMIN\"]\ncutover",
            "\"cap_net_admin\"]\ncutover"
        )),
        Refusal::Syntax(_)
    ));
    assert!(matches!(
        refusal(&with("\"AF_UNIX\"", "\"unix\"")),
        Refusal::Syntax(_)
    ));
    assert!(
        Refusal::Syntax("x".into())
            .to_string()
            .contains("not a declaration")
    );
}

/// Catches a name, an account or a binary outside its grammar. The service name is the grammar
/// of `hostkeeper-config` (design 11.8), which allows upper case and `_`.
#[test]
fn names_and_words_follow_their_grammars() {
    for (from, to, field) in [
        ("name = \"tierstore\"", "name = \"tier store\"", "name"),
        ("name = \"tierstore\"", "name = \"\"", "name"),
        ("binary = \"tierstore\"", "binary = \"../x\"", "binary"),
        ("binary = \"tierstore\"", "binary = \"/x\"", "binary"),
        ("binary = \"tierstore\"", "binary = \"\"", "binary"),
        ("user = \"tierstore-update\"", "user = \"\"", "hook.user"),
        (
            "group = \"tierstore-update\"",
            "group = \"1\"",
            "hook.group",
        ),
    ] {
        assert_eq!(field_of(&refusal(&with(from, to))), field, "{to}");
    }
    assert!(Declaration::parse(&with("name = \"tierstore\"", "name = \"Tier_Store\"")).is_ok());
}

/// Catches a name that keeps its rule but is too long for the units its steps run in
/// (`updater-<name>-<step>-<generation>.service` past 255 bytes): 209 bytes load, 210 and 255
/// are refused on `name`, saying why.
#[test]
fn a_name_too_long_for_its_step_units_is_refused() {
    let fits = "a".repeat(209);
    assert!(
        Declaration::parse(&with("name = \"tierstore\"", &format!("name = \"{fits}\""))).is_ok()
    );
    for name in ["a".repeat(210), "a".repeat(255)] {
        let refused = refusal(&with("name = \"tierstore\"", &format!("name = \"{name}\"")));
        assert_eq!(field_of(&refused), "name");
        assert!(
            refused.to_string().contains(
                "is too long for the units its steps run in: \
                 `updater-<name>-<step>-<generation>.service` would pass 255 bytes"
            ),
            "{refused}"
        );
    }
}

/// Catches a step bound outside 1 s to a day: 0 is a step with no time, and a day and a second
/// is too long.
#[test]
fn step_bounds_stay_within_a_day() {
    let over = (MAX_SECS + 1).to_string();
    for (anchor, step) in [
        ("prepare = 600", "prepare"),
        ("cutover = 300", "cutover"),
        ("verify = 1000", "verify"),
        ("rollback = 600", "rollback"),
        ("cleanup = 1300", "cleanup"),
    ] {
        for n in ["0", over.as_str()] {
            let text = with_in(SAMPLE, anchor, &format!("{step} = {n}"));
            assert_eq!(
                field_of(&refusal(&text)),
                format!("hook.timeout_secs.{step}")
            );
        }
        let day = with_in(SAMPLE, anchor, &format!("{step} = {MAX_SECS}"));
        assert!(Declaration::parse(&day).is_ok(), "{step}");
    }
}

/// Catches `hook_args` carrying an argument systemd-run or the hook could misread, or too many.
#[test]
fn hook_args_are_plain_words() {
    let at = |args: &str| {
        with(
            "name = \"tierstore\"",
            &format!("name = \"tierstore\"\nhook_args = {args}"),
        )
    };
    assert!(Declaration::parse(&at("[\"update-hook\", \"--a=b\"]")).is_ok());
    for bad in ["[\"a b\"]", "[\"\"]", "[\"$x\"]", "[\"a\\\\n\"]"] {
        assert_eq!(field_of(&refusal(&at(bad))), "hook_args", "{bad}");
    }
    let nine = format!("[{}]", ["\"a\""; 9].join(","));
    assert_eq!(field_of(&refusal(&at(&nine))), "hook_args");
    let eight = format!("[{}]", ["\"a\""; 8].join(","));
    assert!(Declaration::parse(&at(&eight)).is_ok());
}

/// Catches a path that systemd would read as something else, in each place a path is written.
#[test]
fn paths_follow_the_grammar_everywhere() {
    for (from, field) in [
        ("\"/var/lib/updater/hooks/tierstore\"", "state_dir"),
        ("\"/usr/sbin/nft\"", "hook.read_paths"),
        ("write_paths = []", "hook.write_paths"),
    ] {
        for bad in [
            "-/x", "+/x", "!/x", "x/y", "/a/../b", "/a b", "/a%h", "/a//b",
        ] {
            let to = match field {
                "hook.write_paths" => format!("write_paths = [\"{bad}\"]"),
                _ => format!("\"{bad}\""),
            };
            match refusal(&with(from, &to)) {
                Refusal::Path {
                    field: found, path, ..
                } => {
                    assert_eq!((found, path.as_str()), (field, bad));
                }
                other => panic!("{field} {bad}: {other:?}"),
            }
        }
    }
    match refusal(&with("\"/etc/tierstore\"", "\"-/etc/tierstore\"")) {
        Refusal::Path { why, .. } => assert_eq!(why, PathError::SystemdPrefix('-')),
        other => panic!("{other:?}"),
    }
}

/// Catches an environment that systemd would split or expand, or a name that is no variable.
#[test]
fn the_environment_is_plain() {
    let at = |env: &str| with("environment = { }", &format!("environment = {env}"));
    assert!(Declaration::parse(&at("{ A = \"b\", C_1 = \"/x:y\" }")).is_ok());
    for bad in [
        "{ \"A B\" = \"x\" }",
        "{ A = \"x y\" }",
        "{ A = \"$HOME\" }",
        "{ \"1A\" = \"x\" }",
        "{ A = \"%h\" }",
    ] {
        assert_eq!(field_of(&refusal(&at(bad))), "hook.environment", "{bad}");
    }
}

/// Catches an environment variable that changes what a program loads or resolves: the loader's
/// (`LD_*`), the character conversion and locale search paths, the allocator's (`MALLOC_*`), the
/// resolver's, and a `PATH` that is not absolute directories only. Harmless names stay.
#[test]
fn the_environment_cannot_hijack_the_runtime() {
    let at = |env: &str| with("environment = { }", &format!("environment = {env}"));
    for name in [
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "LD_AUDIT",
        "LD_DEBUG",
        "LD_BIND_NOW",
        "GCONV_PATH",
        "LOCPATH",
        "NLSPATH",
        "MALLOC_CONF",
        "MALLOC_ARENA_MAX",
        "MALLOC_PERTURB_",
        "HOSTALIASES",
        "RES_OPTIONS",
        "GLIBC_TUNABLES",
        "OPENSSL_CONF",
        "OPENSSL_ENGINES",
        "OPENSSL_MODULES",
        "SSLKEYLOGFILE",
        "ZDOTDIR",
        "GIT_SSH_COMMAND",
        "GIT_EXEC_PATH",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_ASKPASS",
        // What a script run by the hook would load or evaluate before its own text.
        "BASH_ENV",
        "ENV",
        "IFS",
        "CDPATH",
        "SHELLOPTS",
        "BASHOPTS",
        "PS4",
        "PROMPT_COMMAND",
        "PYTHONPATH",
        "PYTHONSTARTUP",
        "PYTHONHOME",
        "PYTHONINSPECT",
        "PYTHONWARNINGS",
        "PERL5LIB",
        "PERL5OPT",
        "PERLLIB",
        "PERL5DB",
        "RUBYLIB",
        "RUBYOPT",
        "NODE_OPTIONS",
        "NODE_PATH",
        "JAVA_TOOL_OPTIONS",
        "_JAVA_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "CLASSPATH",
        "LUA_PATH",
        "LUA_CPATH",
        "TCLLIBPATH",
        "PHPRC",
        "PHP_INI_SCAN_DIR",
        "GEM_PATH",
        "GEM_HOME",
        "DYLD_INSERT_LIBRARIES",
        "DYLD_LIBRARY_PATH",
    ] {
        let text = at(&format!("{{ {name} = \"/x\" }}"));
        assert_eq!(field_of(&refusal(&text)), "hook.environment", "{name}");
    }
    for path in [
        "bin",
        "/usr/bin:bin",
        "",
        ":/usr/bin",
        "/usr/bin:",
        "/usr/bin:.",
        "/usr/../bin",
        "/",
    ] {
        let text = at(&format!("{{ PATH = \"{path}\" }}"));
        let expected = path == "/";
        // `/` alone is an absolute directory.
        assert_eq!(Declaration::parse(&text).is_ok(), expected, "PATH={path:?}");
    }
    for ok in [
        "{ PATH = \"/usr/bin:/usr/sbin\" }",
        "{ PATH = \"/opt/x/bin\" }",
        "{ TMPDIR = \"/run/x\", LANG = \"C\", TZ = \"UTC\" }",
        "{ LDX = \"1\", LDAP_URI = \"x\", MALLOCX = \"1\" }",
        "{ GITHUB_TOKEN = \"x\", ENVIRONMENT = \"x\", IFS_X = \"1\", NODE_ENV = \"production\" }",
    ] {
        assert!(Declaration::parse(&at(ok)).is_ok(), "{ok}");
    }
}

/// Every refusal says what it is about.
#[test]
fn refusals_read_as_sentences() {
    for refusal in [
        Refusal::Value {
            field: "f".into(),
            why: "w".into(),
        },
        Refusal::Path {
            field: "f",
            path: "p".into(),
            why: PathError::Empty,
        },
        Refusal::Reserved {
            field: "f",
            path: "p".into(),
            why: Violation::NotAllowed,
        },
        Refusal::Ancestor {
            field: "f",
            path: "p".into(),
            ancestor: "a".into(),
        },
        Refusal::Conflict {
            what: "a unit",
            with: "other".into(),
        },
    ] {
        assert!(!refusal.to_string().is_empty());
    }
}
