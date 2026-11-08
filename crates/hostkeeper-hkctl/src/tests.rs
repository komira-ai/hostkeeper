use hostkeeper_proto::ctl::{COMMANDS, EnumValue};

use super::*;

/// A field of the tests: a string flag named `name`.
const fn string_field(name: &'static str, flag: &'static str, input: Input) -> Field {
    Field {
        name,
        flag,
        kind: FieldKind::String,
        type_name: "",
        label: Label::Singular,
        input,
        secret: matches!(input, Input::SecretFile),
        enum_values: &[],
        doc: "A field.",
    }
}

/// A command of the tests with `fields`, in the package `package`.
const fn command(package: &'static str, fields: &'static [Field]) -> Command {
    Command {
        package,
        service: "Things",
        method: "Do",
        path: "/x.Things/Do",
        service_command: "things",
        method_command: "do",
        service_doc: "Things.",
        doc: "Do a thing.",
        request: "x.DoRequest",
        response: "x.DoResponse",
        fields,
    }
}

/// Catches a version written into the script that could break its quoted line or run as code:
/// only letters, digits, `.`, `+` and `-` pass, and an empty one does not.
#[test]
fn a_stamp_other_than_a_version_is_refused() {
    for stamp in ["", "1.2.3'", "1 2", "1.2.3+$(id)", "v\n1"] {
        assert_eq!(
            script(COMMANDS, stamp),
            Err(EmitError::Stamp(stamp.to_owned())),
            "{stamp:?}"
        );
    }
    let written = script(COMMANDS, "0.1.70+0123456789ab").expect("a version passes");
    assert!(written.contains("\n_hk_version='hkctl 0.1.70+0123456789ab'\n"));
    assert_eq!(
        EmitError::Stamp("a b".to_owned()).to_string(),
        "the version \"a b\" is not one of letters, digits, `.`, `+` and `-`"
    );
}

/// Catches a request field that takes one of hkctl's own flags (as a flag, or a secret's file or
/// stdin flag) making a script in which the field or the global flag is unreachable; a field
/// that comes only through the request file takes no flag, so it passes.
#[test]
fn a_field_that_takes_one_of_hkctls_own_flags_fails_the_emit() {
    static FLAG: [Field; 1] = [string_field("response_file", "response-file", Input::Flag)];
    static SECRET: [Field; 1] = [string_field(
        "github_oidc_audience",
        "github-oidc-audience",
        Input::SecretFile,
    )];
    static SECRET_STEM: [Field; 1] = [string_field("response", "response", Input::SecretFile)];
    static FILE_ONLY: [Field; 1] = [string_field(
        "response_file",
        "response-file",
        Input::RequestFile,
    )];
    assert_eq!(
        script(&[command("x", &FLAG)], "1"),
        Err(EmitError::FlagClash {
            command: "things do".to_owned(),
            flag: "response-file".to_owned()
        })
    );
    // A secret takes `--<flag>-file` and `--<flag>-stdin`, not its flag itself.
    assert!(script(&[command("x", &SECRET)], "1").is_ok());
    // `response` as a secret takes `--response-file`.
    assert_eq!(
        script(&[command("x", &SECRET_STEM)], "1"),
        Err(EmitError::FlagClash {
            command: "things do".to_owned(),
            flag: "response-file".to_owned()
        })
    );
    assert!(script(&[command("x", &FILE_ONLY)], "1").is_ok());
    // A clash in a host API command does not count: hkctl has no command for it.
    assert!(script(&[command("hostkeeper.agent.v1", &FLAG)], "1").is_ok());
    assert_eq!(
        EmitError::FlagClash {
            command: "things do".to_owned(),
            flag: "response-file".to_owned()
        }
        .to_string(),
        "hkctl things do: the flag --response-file is one of hkctl's own"
    );
}

/// Catches a host API command given to hkctl (the gateway never serves it), or another
/// package's taken away.
#[test]
fn only_the_host_api_has_no_command() {
    assert!(!is_served(&command("hostkeeper.agent.v1", &[])));
    assert!(!is_served(&command("hostkeeper.agent.v2", &[])));
    assert!(is_served(&command("hostkeeper.operator.v1", &[])));
    assert!(is_served(&command("hostkeeper.agentx.v1", &[])));
    assert!(is_served(&command("hostkeeper", &[])));
    let real: Vec<&str> = COMMANDS
        .iter()
        .filter(|c| !is_served(c))
        .map(|c| c.package)
        .collect();
    assert!(!real.is_empty());
    assert!(real.iter().all(|p| *p == "hostkeeper.agent.v1"));
}

/// Catches a field written into the body under a name the gateway does not read, a spec whose
/// parts are out of the runtime's order, and a hold given to a field that is not an integer
/// flag named `wait_secs`.
#[test]
fn a_fields_spec_has_its_json_name_codes_and_values() {
    assert_eq!(json_name("node_id"), "nodeId");
    assert_eq!(json_name("a_b_c"), "aBC");
    assert_eq!(json_name("name"), "name");
    static VALUES: [EnumValue; 2] = [
        EnumValue {
            flag: "red",
            name: "COLOUR_RED",
            number: 1,
        },
        EnumValue {
            flag: "dark_blue",
            name: "COLOUR_DARK_BLUE",
            number: 2,
        },
    ];
    let mut colours = string_field("colours", "colours", Input::Flag);
    colours.kind = FieldKind::Enum;
    colours.label = Label::Repeated;
    colours.enum_values = &VALUES;
    assert_eq!(
        spec(&colours),
        "colours|colours|colours|f|enum|many|red=COLOUR_RED,dark_blue=COLOUR_DARK_BLUE"
    );
    let secret = string_field("pass_word", "pass-word", Input::SecretFile);
    assert_eq!(spec(&secret), "pass-word|pass_word|passWord|s|string|one|");
    let mut blob = string_field("blob", "blob", Input::RequestFile);
    blob.kind = FieldKind::Bytes;
    blob.label = Label::Oneof;
    assert_eq!(spec(&blob), "blob|blob|blob|r|bytes|oneof|");
    let mut inner = blob;
    inner.kind = FieldKind::Message;
    inner.label = Label::Optional;
    assert_eq!(spec(&inner), "blob|blob|blob|r|message|opt|");

    let kinds = [
        (FieldKind::Double, "float"),
        (FieldKind::Float, "float"),
        (FieldKind::Int32, "i32"),
        (FieldKind::Sint32, "i32"),
        (FieldKind::Sfixed32, "i32"),
        (FieldKind::Uint32, "u32"),
        (FieldKind::Fixed32, "u32"),
        (FieldKind::Int64, "i64"),
        (FieldKind::Sint64, "i64"),
        (FieldKind::Sfixed64, "i64"),
        (FieldKind::Uint64, "u64"),
        (FieldKind::Fixed64, "u64"),
        (FieldKind::Bool, "bool"),
        (FieldKind::String, "string"),
        (FieldKind::Bytes, "bytes"),
        (FieldKind::Enum, "enum"),
        (FieldKind::Message, "message"),
    ];
    for (kind, code) in kinds {
        assert_eq!(kind_code(kind), code, "{kind:?}");
        let mut wait = string_field(HOLD_FIELD, "wait-secs", Input::Flag);
        wait.kind = kind;
        assert_eq!(
            is_hold(&wait),
            matches!(code, "i32" | "u32" | "i64" | "u64"),
            "{kind:?}"
        );
    }
    let mut wait = string_field(HOLD_FIELD, "wait-secs", Input::RequestFile);
    wait.kind = FieldKind::Uint32;
    assert!(!is_hold(&wait));
    let mut other = string_field("wait", "wait", Input::Flag);
    other.kind = FieldKind::Uint32;
    assert!(!is_hold(&other));
}

/// Catches a help text that names a field's value or shape wrongly, or loses its doc.
#[test]
fn a_fields_help_names_its_value_and_shape() {
    let mut field = string_field("labels", "labels", Input::Flag);
    field.label = Label::Map;
    assert_eq!(value_name(&field), "KEY=VALUE");
    assert_eq!(shape(&field), "map");
    assert_eq!(
        field_flags(&field),
        [(
            "--labels KEY=VALUE".to_owned(),
            "A field. Repeat the flag for more.".to_owned()
        )]
    );
    field.label = Label::Repeated;
    field.kind = FieldKind::Int64;
    assert_eq!(value_name(&field), "INT64");
    assert_eq!(shape(&field), "repeated int64");
    field.kind = FieldKind::Bool;
    field.label = Label::Optional;
    assert_eq!(value_name(&field), "true|false");
    assert_eq!(shape(&field), "bool");
    field.kind = FieldKind::Message;
    field.type_name = "x.Inner";
    field.label = Label::Oneof;
    assert_eq!(shape(&field), "x.Inner, in a oneof");
    field.label = Label::Singular;
    assert_eq!(shape(&field), "x.Inner");
    field.input = Input::RequestFile;
    assert_eq!(field_flags(&field), []);
}

/// Catches an exit code table that lost a code, maps one twice, or gives `OK` an exit of its
/// own; and a `_hk_exit_of` that does not list every code with its exit.
#[test]
fn every_code_has_one_exit_and_the_script_lists_them() {
    let names: std::collections::BTreeSet<&str> = EXIT_CODES.iter().map(|(n, _)| *n).collect();
    assert_eq!(names.len(), 17);
    assert_eq!(EXIT_CODES[0], ("OK", 0));
    assert!(EXIT_CODES[1..].iter().all(|(_, e)| (1..=3).contains(e)));
    let written = script(COMMANDS, "1").expect("a script");
    assert!(written.contains("\tINVALID_ARGUMENT | FAILED_PRECONDITION) return 2 ;;\n"));
    assert!(written.contains("\tPERMISSION_DENIED | UNAUTHENTICATED) return 3 ;;\n"));
    assert!(written.contains(
        "\tCANCELLED | UNKNOWN | DEADLINE_EXCEEDED | NOT_FOUND | ALREADY_EXISTS | \
         RESOURCE_EXHAUSTED | ABORTED | OUT_OF_RANGE | UNIMPLEMENTED | INTERNAL | UNAVAILABLE | \
         DATA_LOSS) return 1 ;;\n"
    ));
    assert_eq!(MEANINGS.map(|(c, _)| c), [0, 1, 2, 3]);
}

/// Catches a method with no field written as a spec the runtime misreads, a quote in a doc that
/// ends its help text early, and a hold index given to a method without the field.
#[test]
fn a_method_without_fields_and_a_doc_with_a_quote_are_written_whole() {
    let mut empty = command("x", &[]);
    empty.doc = "It's done.";
    let written = script(&[empty], "1").expect("a script");
    assert!(written.contains("\t\t_hk_hold=-1\n\t\t_hk_spec=()\n\t\t;;\n"));
    assert!(
        written.contains(
            "Do a thing."
                .replace("Do a thing.", "It'\\''s done.")
                .as_str()
        )
    );
    assert!(quoted("a'b").eq("'a'\\''b'"));
    assert_eq!(line("a\n  b \n"), "a b");
}
