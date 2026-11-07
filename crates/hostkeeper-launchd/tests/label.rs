//! The launchd labels: their shape, their refusals, and the unit names they are made from.

use hostkeeper_hook::Step;
use hostkeeper_launchd::label::MAX_LABEL;
use hostkeeper_launchd::{Label, LabelError, REVERSE_DNS_PREFIX};

fn unit(service: &str, step: Step, generation: u64) -> String {
    format!("updater-{service}-{}-{generation}", step.name())
}

/// Catches a label that drops the service, the step or the generation, or that differs between
/// two calls with the same arguments (a resumed run would then miss its job).
#[test]
fn labels_follow_prefix_service_step_generation() {
    let label = Label::step("web", Step::Cutover, 7).expect("a valid label");
    assert_eq!(
        label.as_str(),
        format!("{REVERSE_DNS_PREFIX}.hostkeeper.step.web.cutover.7")
    );
    assert_eq!(label.to_string(), label.as_str());
    assert_eq!(Label::step("web", Step::Cutover, 7), Ok(label));
    assert_eq!(
        Label::daemon().as_str(),
        format!("{REVERSE_DNS_PREFIX}.hostkeeper")
    );
}

/// Catches a label with a character launchd or a path would misread, or a length over the limit,
/// and a refusal whose token or message is not the documented one.
#[test]
fn a_label_with_a_bad_character_or_over_255_bytes_is_refused() {
    let longest = "a".repeat(MAX_LABEL);
    assert_eq!(Label::parse(&longest).map(|l| l.to_string()), Ok(longest));
    assert_eq!(
        Label::parse("Ab.c_d-9").map(|l| l.to_string()),
        Ok("Ab.c_d-9".to_owned())
    );

    let too_long = Label::parse(&"a".repeat(MAX_LABEL + 1)).expect_err("256 bytes");
    assert_eq!(too_long, LabelError::TooLong { len: 256 });
    assert_eq!(too_long.reason(), "label_too_long");
    assert_eq!(
        too_long.to_string(),
        "a launchd label is at most 255 bytes, this one is 256"
    );

    let empty = Label::parse("").expect_err("empty");
    assert_eq!(empty, LabelError::Empty);
    assert_eq!(empty.reason(), "label_empty");
    assert_eq!(empty.to_string(), "a launchd label may not be empty");

    let cases = [
        ("a/b", 0x2f_u8, "0x2f"),
        ("a b", 0x20, "0x20"),
        ("a@b", 0x40, "0x40"),
        ("caf\u{e9}", 0xc3, "0xc3"),
    ];
    for (text, byte, hex) in cases {
        let error = Label::parse(text).expect_err(text);
        assert_eq!(
            error,
            LabelError::BadCharacter {
                label: text.to_owned(),
                byte
            }
        );
        assert_eq!(error.reason(), "label_bad_character");
        assert_eq!(
            error.to_string(),
            format!("launchd label {text:?} holds the byte {hex}, which a label may not")
        );
    }

    let service = Label::step("we b", Step::Verify, 1).expect_err("a space in the service");
    assert_eq!(service.reason(), "label_bad_character");
}

/// Catches a unit name read from the left or without its dashes, so that a resumed run's
/// `stop(unit)` names a different job than the one the step started.
#[test]
fn from_unit_round_trips_the_unit_naming_of_every_step() {
    for service in ["web", "my-web-cache", "a-b-1-2"] {
        for step in Step::ALL.into_iter().chain([Step::Uninstall]) {
            let text = unit(service, step, 42);
            assert_eq!(
                Label::from_unit(&text),
                Label::step(service, step, 42),
                "{text}"
            );
        }
    }
    assert_eq!(
        Label::from_unit(&unit("web", Step::Prepare, u64::MAX)),
        Label::step("web", Step::Prepare, u64::MAX)
    );
}

/// Catches a malformed unit name that is turned into some label instead of being refused.
#[test]
fn from_unit_refuses_a_malformed_unit() {
    let bad = [
        "web-cutover-7",
        "updater-7",
        "updater-web-cutover-",
        "updater-web-cutover-x7",
        "updater-web-cutover-+7",
        "updater-web-cutover-99999999999999999999",
        "updater-web-frobnicate-7",
        "updater-cutover-7",
        "updater--cutover-7",
        "",
    ];
    for text in bad {
        let error = Label::from_unit(text).expect_err(text);
        assert_eq!(
            error,
            LabelError::NotAStepUnit {
                unit: text.to_owned()
            }
        );
        assert_eq!(error.reason(), "label_not_a_step_unit");
        assert_eq!(
            error.to_string(),
            format!(
                "{text:?} is not the name of a step unit (updater-<service>-<step>-<generation>)"
            )
        );
    }
}

/// Catches a probe label that could be a step's (a probe would then boot out a step job of the
/// same name, or the other way round) or that takes what a label may not hold: the label of a
/// probe is `<prefix>.hostkeeper.probe.<unit>`, apart from the step label of the same unit text,
/// and a unit with a byte a label refuses, or too long for one, is refused as a label is.
#[test]
fn a_probe_label_is_apart_from_every_step_label_and_refuses_what_a_label_refuses() {
    assert_eq!(
        Label::probe("hostkeeper-self-update-protocol")
            .expect("a probe label")
            .as_str(),
        "dev.hostkeeper.hostkeeper.probe.hostkeeper-self-update-protocol"
    );
    let unit = unit("web", Step::Cutover, 7);
    assert_ne!(
        Label::probe(&unit).expect("a probe label"),
        Label::from_unit(&unit).expect("a step label")
    );
    assert_eq!(
        Label::probe("a/b"),
        Err(LabelError::BadCharacter {
            label: "dev.hostkeeper.hostkeeper.probe.a/b".to_owned(),
            byte: b'/',
        })
    );
    let long = "x".repeat(MAX_LABEL);
    assert_eq!(
        Label::probe(&long),
        Err(LabelError::TooLong {
            len: "dev.hostkeeper.hostkeeper.probe.".len() + MAX_LABEL
        })
    );
}

/// Catches the unit of a `[hook] boot` step refused as no step unit (the daemon's boot pass
/// stops `updater-<service>-boot-<generation>` and runs its additive step in it, so on a Mac
/// every boot step failed before it started): it gives the step label with the word `boot`,
/// apart from every other step's label of the same update, and a word that is neither a step
/// nor `boot` is still refused.
#[test]
fn from_unit_gives_the_unit_of_a_boot_step_its_own_label() {
    let label = Label::from_unit("updater-web-api-boot-7").expect("a boot unit");
    assert_eq!(
        label.as_str(),
        format!("{REVERSE_DNS_PREFIX}.hostkeeper.step.web-api.boot.7")
    );
    for step in Step::ALL {
        assert_ne!(
            Label::from_unit(&unit("web-api", step, 7)),
            Ok(label.clone())
        );
    }
    assert_eq!(
        Label::from_unit("updater-web-reboot-7"),
        Err(LabelError::NotAStepUnit {
            unit: "updater-web-reboot-7".to_owned()
        })
    );
}
