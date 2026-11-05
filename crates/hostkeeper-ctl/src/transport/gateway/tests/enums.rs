//! The refusal of an enum name this build does not know, in a field it does know: through the
//! fixture's response type, and on a pool built here for the nested and map shapes.

use prost::Message as _;
use prost_reflect::prost_types::field_descriptor_proto::{Label, Type};
use prost_reflect::prost_types::{
    DescriptorProto, EnumDescriptorProto, EnumValueDescriptorProto, FieldDescriptorProto,
    FileDescriptorProto, FileDescriptorSet, MessageOptions,
};
use prost_reflect::{DescriptorPool, MessageDescriptor};
use tonic::Code;
use tonic::metadata::MetadataMap;

use super::{FakeStream, call, response};
use crate::testutil::from_json;
use crate::transport::gateway::enums::check;

/// One field of the pool built here.
fn field(
    name: &str,
    json: &str,
    number: i32,
    label: Label,
    kind: Type,
    type_name: Option<&str>,
) -> FieldDescriptorProto {
    FieldDescriptorProto {
        name: Some(name.to_owned()),
        json_name: Some(json.to_owned()),
        number: Some(number),
        label: Some(label as i32),
        r#type: Some(kind as i32),
        type_name: type_name.map(str::to_owned),
        ..Default::default()
    }
}

/// A map entry type: a string key and a value of `kind`.
fn entry(name: &str, kind: Type, type_name: &str) -> DescriptorProto {
    DescriptorProto {
        name: Some(name.to_owned()),
        field: vec![
            field("key", "key", 1, Label::Optional, Type::String, None),
            field("value", "value", 2, Label::Optional, kind, Some(type_name)),
        ],
        options: Some(MessageOptions {
            map_entry: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The message `Root` of a pool with an enum `Tone`: a singular enum, a message with one, a
/// list of those, a map of enums and a map of messages.
fn root() -> MessageDescriptor {
    let tone = EnumDescriptorProto {
        name: Some("Tone".to_owned()),
        value: ["TONE_UNSPECIFIED", "TONE_WARM"]
            .iter()
            .zip(0..)
            .map(|(name, number)| EnumValueDescriptorProto {
                name: Some((*name).to_owned()),
                number: Some(number),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    let leaf = DescriptorProto {
        name: Some("Leaf".to_owned()),
        field: vec![field(
            "tone",
            "tone",
            1,
            Label::Optional,
            Type::Enum,
            Some(".tones.Tone"),
        )],
        ..Default::default()
    };
    let root = DescriptorProto {
        name: Some("Root".to_owned()),
        field: vec![
            field(
                "tone_now",
                "toneNow",
                1,
                Label::Optional,
                Type::Enum,
                Some(".tones.Tone"),
            ),
            field(
                "leaf",
                "leaf",
                2,
                Label::Optional,
                Type::Message,
                Some(".tones.Leaf"),
            ),
            field(
                "leaves",
                "leaves",
                3,
                Label::Repeated,
                Type::Message,
                Some(".tones.Leaf"),
            ),
            field(
                "by_name",
                "byName",
                4,
                Label::Repeated,
                Type::Message,
                Some(".tones.Root.ByNameEntry"),
            ),
            field(
                "leaf_by_name",
                "leafByName",
                5,
                Label::Repeated,
                Type::Message,
                Some(".tones.Root.LeafByNameEntry"),
            ),
        ],
        nested_type: vec![
            entry("ByNameEntry", Type::Enum, ".tones.Tone"),
            entry("LeafByNameEntry", Type::Message, ".tones.Leaf"),
        ],
        ..Default::default()
    };
    let file = FileDescriptorProto {
        name: Some("tones.proto".to_owned()),
        package: Some("tones".to_owned()),
        syntax: Some("proto3".to_owned()),
        enum_type: vec![tone],
        message_type: vec![leaf, root],
        ..Default::default()
    };
    DescriptorPool::from_file_descriptor_set(FileDescriptorSet { file: vec![file] })
        .expect("the pool")
        .get_message_by_name("tones.Root")
        .expect("Root")
}

/// The refusal for `name` at `path`.
fn unknown(name: &str, path: &str) -> String {
    format!(
        "the reply has {name:?} at {path}, a value of tones.Tone that this build of hostkeeper \
         does not know; a newer hostkeeper, or --transport grpc, reads it"
    )
}

/// Catches an unknown enum name dropped silently, in a singular field, a list element, a message
/// in a message, a message in a list, a map value and a message in a map value; and the path
/// that names it wrong, or a key named by its proto spelling not checked.
#[test]
fn an_unknown_enum_name_is_named_wherever_it_is() {
    let root = root();
    let cold = "TONE_COLD";
    for (json, path) in [
        (r#"{"toneNow": "TONE_COLD"}"#, "toneNow"),
        (r#"{"tone_now": "TONE_COLD"}"#, "tone_now"),
        (r#"{"leaf": {"tone": "TONE_COLD"}}"#, "leaf.tone"),
        (
            r#"{"leaves": [{"tone": "TONE_WARM"}, {"tone": "TONE_COLD"}]}"#,
            "leaves[1].tone",
        ),
        (r#"{"byName": {"k": "TONE_COLD"}}"#, r#"byName["k"]"#),
        (
            r#"{"leafByName": {"a": {"tone": "TONE_COLD"}}}"#,
            r#"leafByName["a"].tone"#,
        ),
        (
            r#"{"unknownKey": 1, "leaf": {"unknownKey": [], "tone": "TONE_COLD"}}"#,
            "leaf.tone",
        ),
    ] {
        let value = serde_json::from_str(json).expect(json);
        assert_eq!(check(&value, &root, ""), Err(unknown(cold, path)), "{json}");
    }
}

/// Catches a known value, a number, null, a key this build does not know, or a value of the
/// wrong JSON shape refused by the check, which leaves the rest to the reader.
#[test]
fn known_values_numbers_and_unknown_keys_pass() {
    let root = root();
    for json in [
        "{}",
        "[]",
        r#"{"toneNow": "TONE_WARM", "tone_now": "TONE_UNSPECIFIED"}"#,
        r#"{"toneNow": 7}"#,
        r#"{"toneNow": null, "leaf": null, "leaves": null, "byName": null}"#,
        r#"{"fieldOfANewerServer": "TONE_COLD", "futureEnum": ["NEW_VALUE"]}"#,
        r#"{"leaf": {"fieldOfANewerServer": {"tone": "TONE_COLD"}}}"#,
        r#"{"leaves": "TONE_COLD", "byName": ["TONE_COLD"], "leaf": "TONE_COLD"}"#,
        r#"{"leaves": [3, null, "x"], "byName": {"k": 1, "j": null}}"#,
        r#"{"leafByName": {"a": null, "b": {}}}"#,
    ] {
        let value = serde_json::from_str(json).expect(json);
        assert_eq!(check(&value, &root, ""), Ok(()), "{json}");
    }
}

/// Catches a reply with an unknown colour, singular or inside a list, returned as a success with
/// the value left out; a colour given as a number, and fields of a newer server, refused.
#[test]
fn the_gateway_refuses_an_unknown_colour_but_reads_newer_fields() {
    let refusal = |name: &str, path: &str| {
        format!(
            "the reply has {name:?} at {path}, a value of hostkeeper.fixture.v1.Colour that \
             this build of hostkeeper does not know; a newer hostkeeper, or --transport grpc, \
             reads it"
        )
    };
    for (json, path) in [
        (r#"{"colour": "COLOUR_GREEN"}"#, "colour"),
        (
            r#"{"colours": ["COLOUR_RED", "COLOUR_GREEN"]}"#,
            "colours[1]",
        ),
    ] {
        let mut stream = FakeStream::replying(&response("200 OK", json));
        let status =
            call(&mut stream, &from_json("Widget", "{}"), &MetadataMap::new()).expect_err(json);
        assert_eq!(status.code(), Code::Internal, "{json}");
        assert_eq!(status.message(), refusal("COLOUR_GREEN", path), "{json}");
    }

    let mut stream = FakeStream::replying(&response(
        "200 OK",
        r#"{"colour": 7, "colours": ["COLOUR_RED", 2], "newerField": {"colour": "COLOUR_GREEN"}}"#,
    ));
    let reply = call(&mut stream, &from_json("Widget", "{}"), &MetadataMap::new())
        .expect("numbers and unknown fields are read");
    assert_eq!(
        reply,
        from_json("Widget", r#"{"colour": 7, "colours": ["COLOUR_RED", 2]}"#).encode_to_vec()
    );
}
