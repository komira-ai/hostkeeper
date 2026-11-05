//! The claims of a Mac: its serial number and platform UUID read from the text of `ioreg`, the
//! values refused, and an enrollment that posts nothing when it has no serial to claim.

use std::error::Error as _;
use std::io;

use hostkeeper_enroll::{
    Claims, ClaimsError, ClaimsSource, Enroller, Error, Ioreg, MacClaims, MacIds, Outcome,
    parse_ioreg,
};

use crate::support::World;

const SERIAL_LINE: &str = r#""IOPlatformSerialNumber" = "C02TEST0ABCD""#;
const UUID_LINE: &str = r#""IOPlatformUUID" = "12345678-90AB-CDEF-1234-567890ABCDEF""#;

/// `ioreg` output with `serial_line` and `uuid_line` among other keys, each left out when empty.
/// Keys that only begin like the two (`IOPlatformSerialNumberX`) and a `serial-number` data line
/// come first, so a reader that matches on a prefix of the key takes the wrong value.
fn output(serial_line: &str, uuid_line: &str) -> String {
    let mut text = String::from(
        "+-o J314sAP  <class IOPlatformExpertDevice, id 0x100000225, registered, matched, active>\n\
         \x20 {\n\
         \x20   \"IOPlatformSerialNumberX\" = \"DECOY0000001\"\n\
         \x20   \"serial-number\" = <433032544553543041424344000000000000000000000000>\n\
         \x20   \"manufacturer\" = <\"Apple Inc.\">\n\
         \x20   \"IOPlatformUUIDX\" = \"00000000-0000-0000-0000-000000000000\"\n",
    );
    for line in [serial_line, uuid_line] {
        if !line.is_empty() {
            text.push_str("    ");
            text.push_str(line);
            text.push('\n');
        }
    }
    text.push_str("    \"model\" = <\"MacBookPro18,3\">\n  }\n\n");
    text
}

/// An `ioreg` that prints `text`, or cannot be run when it has none.
struct FakeIoreg(Option<String>);

impl Ioreg for FakeIoreg {
    fn platform_expert(&self) -> io::Result<String> {
        match &self.0 {
            Some(text) => Ok(text.clone()),
            None => Err(io::Error::new(io::ErrorKind::NotFound, "ioreg not found")),
        }
    }
}

fn mac_claims(text: String) -> MacClaims {
    MacClaims::new(Box::new(FakeIoreg(Some(text))))
}

/// An enroller of `world` whose claims are the Mac claims of `text`.
fn mac_enroller(world: &World, text: String) -> Enroller {
    let mut ports = world.ports_with(None, None);
    ports.claims = Box::new(mac_claims(text));
    Enroller::open(world.settings(), ports).expect("open the enroller")
}

fn mac_only() -> Claims {
    Claims {
        machine_id: None,
        product_serial: None,
        product_uuid: None,
        network_addresses: Vec::new(),
        mac_serial: Some("C02TEST0ABCD".into()),
        platform_uuid: Some("12345678-90AB-CDEF-1234-567890ABCDEF".into()),
        bmc_mac: None,
    }
}

/// Catches a serial or UUID taken from a key that only begins like it, from a data line, or
/// refused for its letter case.
#[test]
fn parse_ioreg_reads_serial_and_platform_uuid() {
    let ids = parse_ioreg(&output(SERIAL_LINE, UUID_LINE)).expect("the ids");
    assert_eq!(
        ids,
        MacIds {
            serial: "C02TEST0ABCD".into(),
            platform_uuid: "12345678-90AB-CDEF-1234-567890ABCDEF".into(),
        }
    );
    let lower = output(
        SERIAL_LINE,
        r#""IOPlatformUUID" = "abcdef01-2345-6789-abcd-ef0123456789""#,
    );
    assert_eq!(
        parse_ioreg(&lower)
            .expect("a lower-case uuid")
            .platform_uuid,
        "abcdef01-2345-6789-abcd-ef0123456789"
    );
}

/// Catches an enrollment that posts a request for a Mac with no serial, an empty one or one that
/// is not a quoted string: the step fails with the reason and the coordinator is never called.
#[test]
fn ioreg_output_without_a_serial_or_with_an_empty_one_stops_enrollment() {
    let cases = [
        (
            "mac-no-serial",
            output("", UUID_LINE),
            "the host's claims: ioreg output has no IOPlatformSerialNumber",
        ),
        (
            "mac-empty-serial",
            output(r#""IOPlatformSerialNumber" = """#, UUID_LINE),
            "the host's claims: ioreg output has an empty IOPlatformSerialNumber",
        ),
        (
            "mac-unquoted-serial",
            output(r#""IOPlatformSerialNumber" = C02TEST0ABCD"#, UUID_LINE),
            "the host's claims: ioreg output has a malformed IOPlatformSerialNumber",
        ),
    ];
    for (name, text, message) in cases {
        let world = World::new(name);
        let mut enroller = mac_enroller(&world, text);
        let refused = enroller.run_once().expect_err("no claims");
        assert!(matches!(&refused, Error::Claims(_)), "{refused:?}");
        assert_eq!(refused.to_string(), message);
        assert!(world.coordinator().enrolls.is_empty(), "{name}");
        assert!(world.coordinator().polls.is_empty(), "{name}");
    }
}

/// Catches Linux fields filled on a Mac, a Mac field lost on the way to the request, or a serial
/// and UUID that are not the request's claims.
#[test]
fn mac_claims_carry_serial_and_platform_uuid_and_no_linux_fields() {
    let claims = mac_claims(output(SERIAL_LINE, UUID_LINE))
        .claims()
        .expect("the claims");
    assert_eq!(claims, mac_only());

    let world = World::new("mac-claims-request");
    let mut enroller = mac_enroller(&world, output(SERIAL_LINE, UUID_LINE));
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    let c = world.coordinator();
    assert_eq!(c.enrolls.len(), 1);
    assert_eq!(c.enrolls[0].claims, mac_only());
}

/// Catches each value the parser must refuse being accepted or reported as another kind of
/// fault, and an `ioreg` that cannot be run being hidden.
#[test]
fn parse_ioreg_refuses_each_malformed_value() {
    let serial = |value: &str| output(&format!("\"IOPlatformSerialNumber\" = {value}"), UUID_LINE);
    let uuid = |value: &str| output(SERIAL_LINE, &format!("\"IOPlatformUUID\" = {value}"));
    let twice = output(&format!("{SERIAL_LINE}\n    {SERIAL_LINE}"), UUID_LINE);
    let long = "A".repeat(65);
    let cases = [
        (
            output(SERIAL_LINE, ""),
            "ioreg output has no IOPlatformUUID",
        ),
        (
            twice,
            "ioreg output has more than one IOPlatformSerialNumber",
        ),
        (uuid(r#""""#), "ioreg output has an empty IOPlatformUUID"),
        (uuid(r#""   ""#), "ioreg output has an empty IOPlatformUUID"),
        (
            uuid(r#""12345678-90AB-CDEF-1234-567890ABCDE""#),
            "ioreg output has a malformed IOPlatformUUID",
        ),
        (
            uuid(r#""12345678-90AB-CDEF-1234-567890ABCDEG""#),
            "ioreg output has a malformed IOPlatformUUID",
        ),
        (
            uuid(r#""1234567-890AB-CDEF-1234-567890ABCDEF""#),
            "ioreg output has a malformed IOPlatformUUID",
        ),
        (
            uuid(r#""12345678-90AB-CDEF-1234-567890ABCDEF" extra"#),
            "ioreg output has a malformed IOPlatformUUID",
        ),
        (
            serial(r#""C02 TEST0ABC""#),
            "ioreg output has a malformed IOPlatformSerialNumber",
        ),
        (
            serial("\"C02TEST0ABC\u{e9}\""),
            "ioreg output has a malformed IOPlatformSerialNumber",
        ),
        (
            serial(&format!("\"{long}\"")),
            "ioreg output has a malformed IOPlatformSerialNumber",
        ),
        (
            serial(r#""C02TEST0ABCD" extra"#),
            "ioreg output has a malformed IOPlatformSerialNumber",
        ),
        (
            serial(r#"C02TEST0ABCD""#),
            "ioreg output has a malformed IOPlatformSerialNumber",
        ),
        (
            serial(r#""C02TEST0ABCD"#),
            "ioreg output has a malformed IOPlatformSerialNumber",
        ),
        (
            serial(r#""  ""#),
            "ioreg output has an empty IOPlatformSerialNumber",
        ),
        (String::new(), "ioreg output has no IOPlatformSerialNumber"),
    ];
    for (text, message) in cases {
        let refused = parse_ioreg(&text).expect_err(message);
        assert_eq!(refused.to_string(), message);
    }

    let longest = "A".repeat(64);
    let ids = parse_ioreg(&serial(&format!("\"{longest}\""))).expect("64 bytes");
    assert_eq!(ids.serial, longest);

    let unreadable = MacClaims::new(Box::new(FakeIoreg(None)));
    let refused = unreadable.claims().expect_err("no ioreg");
    assert!(
        matches!(&refused, ClaimsError::Unreadable(_)),
        "{refused:?}"
    );
    assert_eq!(
        refused.to_string(),
        "ioreg could not be read: ioreg not found"
    );
    assert!(refused.source().is_some());
    assert!(ClaimsError::Missing("x").source().is_none());
}

/// Catches identifiers or a claims error that print as something else in a log: the debug text
/// names each field and value.
#[test]
fn mac_ids_and_a_claims_error_print_their_fields() {
    let ids = parse_ioreg(&output(SERIAL_LINE, UUID_LINE)).expect("the two values");
    assert_eq!(
        format!("{ids:?}"),
        r#"MacIds { serial: "C02TEST0ABCD", platform_uuid: "12345678-90AB-CDEF-1234-567890ABCDEF" }"#
    );
    assert_eq!(
        format!("{:?}", ClaimsError::Missing("IOPlatformUUID")),
        r#"Missing("IOPlatformUUID")"#
    );
}
