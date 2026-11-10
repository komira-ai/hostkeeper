//! The reading of hkctl's command line on a fixture table that holds one field of every kind,
//! and the global flags on `docs/cli.md`'s table as `hostkeeper_hkctl::markdown` writes it.

use hostkeeper_proto::ctl::{COMMANDS, Command, EnumValue, Field, FieldKind, Input, Label};

use super::{Global, Parsed, globals_of, parse, served};

/// A request field of the fixture.
const fn field(name: &'static str, flag: &'static str, kind: FieldKind, label: Label) -> Field {
    Field {
        name,
        flag,
        kind,
        type_name: "",
        label,
        input: Input::Flag,
        secret: false,
        enum_values: &[],
        doc: "",
    }
}

const STATES: &[EnumValue] = &[
    EnumValue {
        flag: "on",
        name: "STATE_ON",
        number: 1,
    },
    EnumValue {
        flag: "off",
        name: "STATE_OFF",
        number: 2,
    },
];

const FIELDS: &[Field] = &[
    field("name", "name", FieldKind::String, Label::Singular),
    field("force", "force", FieldKind::Bool, Label::Optional),
    Field {
        enum_values: STATES,
        ..field("state", "state", FieldKind::Enum, Label::Singular)
    },
    field("count", "count", FieldKind::Int32, Label::Singular),
    field("wait_secs", "wait-secs", FieldKind::Uint32, Label::Singular),
    field("offset", "offset", FieldKind::Sint64, Label::Singular),
    field("size", "size", FieldKind::Fixed64, Label::Singular),
    field("ratio", "ratio", FieldKind::Double, Label::Singular),
    field("tags", "tags", FieldKind::String, Label::Repeated),
    field("labels", "labels", FieldKind::Message, Label::Map),
    Field {
        input: Input::SecretFile,
        secret: true,
        ..field("password", "password", FieldKind::String, Label::Singular)
    },
    Field {
        input: Input::RequestFile,
        ..field("plan", "plan", FieldKind::Message, Label::Singular)
    },
];

/// A fixture command.
const fn command(package: &'static str, service: &'static str, method: &'static str) -> Command {
    Command {
        package,
        service: "",
        method: "",
        path: "",
        service_command: service,
        method_command: method,
        service_doc: "",
        doc: "",
        request: "",
        response: "",
        fields: FIELDS,
    }
}

const FIXTURE: &[Command] = &[
    command("hostkeeper.operator.v1", "things", "put"),
    command("hostkeeper.operator.v1", "things", "list"),
    // The host API's: hkctl has no command for it.
    command("hostkeeper.agent.v1", "coordinator", "heartbeat"),
];

/// hkctl's global flags, from the real `docs/cli.md` text.
fn globals() -> Vec<Global> {
    globals_of(&hostkeeper_hkctl::markdown(COMMANDS)).expect("the global flags")
}

/// The fixture line after `hkctl`, with the globals a call needs before `rest`.
fn call(rest: &str) -> Result<Parsed, String> {
    line(&format!(
        "--endpoint https://c.example.com --ca-file ca.pem --token-file t {rest}"
    ))
}

/// The fixture line after `hkctl`, split by white space.
fn line(text: &str) -> Result<Parsed, String> {
    let words: Vec<String> = text.split_whitespace().map(str::to_owned).collect();
    parse(&served(FIXTURE), &globals(), &words)
}

/// Catches the global flags read from the wrong table, a switch read as taking a value, a flag
/// `hostkeeper ctl` has and hkctl does not, and a text with no table read as having one.
#[test]
fn the_global_flags_are_cli_mds() {
    let names: Vec<(String, bool)> = globals().into_iter().map(|g| (g.name, g.value)).collect();
    let expected: Vec<(String, bool)> = [
        ("endpoint", true),
        ("ca-file", true),
        ("token-file", true),
        ("token-stdin", false),
        ("github-oidc-audience", true),
        ("response-file", true),
        ("timeout-secs", true),
    ]
    .into_iter()
    .map(|(n, v)| (n.to_owned(), v))
    .collect();
    assert_eq!(names, expected);
    assert_eq!(
        globals_of("# hkctl\n\nno table\n"),
        Err("no section `## Global flags`".to_owned())
    );
    assert_eq!(
        globals_of("## Global flags\n\nnone\n\n## Exit codes\n| `--x` | V |\n"),
        Err("the section `## Global flags` has no flags".to_owned())
    );
}

/// Catches a line that hkctl takes being refused: every kind of flag with a value it takes, both
/// spellings of a value, a repeated flag and a map repeated, a secret from a file, the globals on
/// either side of the service and after the method, and help and version.
#[test]
fn every_kind_of_flag_is_taken() {
    let put = Ok(Parsed::Call("things put".to_owned()));
    for rest in [
        "things put",
        "things put --name n --force false --state on --count -2147483648",
        "things put --wait-secs 4294967295 --offset -9223372036854775808",
        "things put --size 18446744073709551615 --count 007 --wait-secs -0",
        "things put --ratio 1.5",
        "things put --ratio=-2e-3",
        "things put --tags a --tags b --labels k=v --labels j=",
        "things put --password-file p --request r.json --name=--dashes",
        "things put --ratio .5",
        "things put --ratio NaN",
        "things put --ratio 1.",
        "things put --ratio 3E+7",
        "things put --timeout-secs 999999 --response-file out.json",
    ] {
        assert_eq!(call(rest), put, "{rest}");
    }
    assert_eq!(
        line(
            "--endpoint=https://c.example.com --ca-file ca things --token-stdin put --password-file p"
        ),
        put
    );
    assert_eq!(
        line("--endpoint https://c.example.com --ca-file ca things put --github-oidc-audience a"),
        put
    );
    for help in [
        "--help",
        "-h",
        "things --help",
        "things put --help",
        "things put --name n -h",
    ] {
        assert_eq!(line(help), Ok(Parsed::Help), "{help}");
    }
    for endpoint in [
        "https://c.example.com",
        "https://c.example.com/",
        "https://c.example.com:8443",
        "https://[2001:db8::1]:443/",
        "https://[2001:db8::1]",
        "https://<private-listener>",
        "https://<private-listener>:8443",
    ] {
        assert_eq!(
            line(&format!(
                "--endpoint {endpoint} --ca-file c --token-file t things put"
            )),
            put,
            "{endpoint}"
        );
    }
    assert_eq!(line("--version"), Ok(Parsed::Version));
    assert_eq!(
        line("--endpoint https://c.example.com --version"),
        Ok(Parsed::Version)
    );
}

/// Catches a line the script refuses as a usage error being taken: each refusal below is the
/// script's, worded as it words it.
#[test]
fn a_line_hkctl_refuses_is_refused() {
    for (rest, error) in [
        ("thing put", "no service thing; 'hkctl --help' lists them"),
        (
            "coordinator heartbeat",
            "no service coordinator; 'hkctl --help' lists them",
        ),
        (
            "things get",
            "the service things has no method get; 'hkctl things --help' lists them",
        ),
        ("things put --nam n", "things put has no flag --nam"),
        ("things put --plan p", "things put has no flag --plan"),
        (
            "things put --password p",
            "things put has no flag --password",
        ),
        (
            "things put n",
            "things put takes no argument; it has flags only",
        ),
        ("things put --force yes", "--force takes true or false"),
        (
            "things put --state unspecified",
            "--state takes one of: on, off",
        ),
        (
            "things put --count 2147483648",
            "--count takes a 32-bit signed integer",
        ),
        (
            "things put --count 1.5",
            "--count takes a 32-bit signed integer",
        ),
        (
            "things put --count -",
            "--count takes a 32-bit signed integer",
        ),
        (
            "things put --wait-secs -1",
            "--wait-secs takes a 32-bit unsigned integer",
        ),
        (
            "things put --offset 9223372036854775808",
            "--offset takes a 64-bit signed integer",
        ),
        (
            "things put --size 18446744073709551616",
            "--size takes a 64-bit unsigned integer",
        ),
        (
            "things put --size 99999999999999999999999999999999999999999",
            "--size takes a 64-bit unsigned integer",
        ),
        ("things put --ratio 1.2.3", "--ratio takes a number"),
        ("things put --ratio e5", "--ratio takes a number"),
        ("things put --ratio 1e", "--ratio takes a number"),
        ("things put --ratio .", "--ratio takes a number"),
        ("things put --labels k", "--labels takes key=value"),
        (
            "things put --labels k=1 --labels k=2",
            "--labels names the key \"k\" twice",
        ),
        ("things put --name a --name b", "--name is given twice"),
        (
            "things put --password-file p --password-stdin",
            "--password-file and --password-stdin: give one, once",
        ),
        (
            "things put --password-stdin=x",
            "--password-stdin takes no value",
        ),
        ("things put --name", "--name needs a value"),
        ("things put --name --force true", "--name needs a value"),
        (
            "things put --request a --request b",
            "--request is given twice",
        ),
        (
            "things put --request - --password-stdin",
            "--request and --password-stdin both read stdin; at most one input may",
        ),
        (
            "things put --token-stdin",
            "a call needs exactly one of --token-file, --token-stdin and --github-oidc-audience",
        ),
        ("things put --ca-file other", "--ca-file is given twice"),
        (
            "things put --timeout-secs 0",
            "--timeout-secs takes a whole number of seconds, 1 to 999999",
        ),
        (
            "things put --timeout-secs 1000000",
            "--timeout-secs takes a whole number of seconds, 1 to 999999",
        ),
        ("things --output json put", "unknown flag --output"),
        ("--transport=gateway things put", "unknown flag --transport"),
        (
            "things put --output json",
            "things put has no flag --output",
        ),
        ("things --version put", "unknown flag --version"),
    ] {
        assert_eq!(call(rest), Err(error.to_owned()), "{rest}");
    }
    for (text, error) in [
        ("", "no service: hkctl <service> <method> [flags]"),
        ("things", "no method: hkctl things <method> [flags]"),
        (
            "--ca-file c --token-file t things put",
            "a call needs --endpoint",
        ),
        (
            "--endpoint http://c.example.com --ca-file c --token-file t things put",
            "--endpoint takes https://<host>[:<port>]",
        ),
        (
            "--endpoint https://c.example.com --token-file t things put",
            "a call needs --ca-file",
        ),
        (
            "--endpoint https://c.example.com --ca-file c things put",
            "a call needs exactly one of --token-file, --token-stdin and --github-oidc-audience",
        ),
        (
            "--token-stdin --token-stdin things put",
            "--token-stdin is given twice",
        ),
        (
            "--token-stdin things put --request -",
            "--token-stdin and --request both read stdin; at most one input may",
        ),
        ("--endpoint", "--endpoint needs a value"),
        (
            "--endpoint https:// --ca-file c --token-file t things put",
            "--endpoint takes https://<host>[:<port>]",
        ),
        (
            "--endpoint https://c.example.com/v1 --ca-file c --token-file t things put",
            "--endpoint takes https://<host>[:<port>]",
        ),
        (
            "--endpoint https://c.example.com:123456 --ca-file c --token-file t things put",
            "--endpoint takes https://<host>[:<port>]",
        ),
        (
            "--endpoint https://c.example.com:port --ca-file c --token-file t things put",
            "--endpoint takes https://<host>[:<port>]",
        ),
        (
            "--endpoint https://c.example.com: --ca-file c --token-file t things put",
            "--endpoint takes https://<host>[:<port>]",
        ),
        (
            "--endpoint https://[2001:db8::1]x --ca-file c --token-file t things put",
            "--endpoint takes https://<host>[:<port>]",
        ),
        (
            "--endpoint https://[] --ca-file c --token-file t things put",
            "--endpoint takes https://<host>[:<port>]",
        ),
        (
            "--endpoint https://[2001:db8::1 --ca-file c --token-file t things put",
            "--endpoint takes https://<host>[:<port>]",
        ),
        (
            "--endpoint https://c_d.example.com --ca-file c --token-file t things put",
            "--endpoint takes https://<host>[:<port>]",
        ),
        (
            "--endpoint https://<> --ca-file c --token-file t things put",
            "--endpoint takes https://<host>[:<port>]",
        ),
    ] {
        assert_eq!(line(text), Err(error.to_owned()), "{text}");
    }
}

/// Catches the real table read without the host API left out, or with a service of it missing.
#[test]
fn the_real_table_is_hkctls() {
    let served = served(COMMANDS);
    assert!(
        served
            .iter()
            .all(|c| !c.package.starts_with("hostkeeper.agent."))
    );
    for command in ["enrollment approve", "hosts retire", "releases status"] {
        assert!(
            served
                .iter()
                .any(|c| format!("{} {}", c.service_command, c.method_command) == command),
            "{command}"
        );
    }
}
