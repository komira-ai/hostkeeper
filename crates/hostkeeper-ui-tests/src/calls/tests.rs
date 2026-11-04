use prost_types::{FileDescriptorProto, MethodDescriptorProto, ServiceDescriptorProto};

use super::*;

/// A descriptor set of one file: `package` with `Hosts` (`List`, `Get`).
fn one_file(package: &str) -> FileDescriptorSet {
    let method = |name: &str| MethodDescriptorProto {
        name: Some(name.to_owned()),
        ..MethodDescriptorProto::default()
    };
    FileDescriptorSet {
        file: vec![FileDescriptorProto {
            package: Some(package.to_owned()),
            service: vec![ServiceDescriptorProto {
                name: Some("Hosts".to_owned()),
                method: vec![method("List"), method("Get")],
                ..ServiceDescriptorProto::default()
            }],
            ..FileDescriptorProto::default()
        }],
    }
}

fn names(calls: &[&str]) -> Vec<String> {
    calls.iter().map(|c| (*c).to_owned()).collect()
}

#[test]
fn the_descriptor_methods_are_package_service_and_method() {
    let methods: Vec<String> = descriptor_methods(&one_file("a.v1")).into_iter().collect();
    assert_eq!(methods, ["a.v1.Hosts/Get", "a.v1.Hosts/List"]);
}

#[test]
fn check_calls_accepts_known_methods_and_names_each_unknown_one_once() {
    let set = one_file("a.v1");
    assert_eq!(
        check_calls(&names(&["a.v1.Hosts/List", "a.v1.Hosts/Get"]), &set),
        Ok(())
    );
    let unknown = check_calls(
        &names(&[
            "a.v1.Hosts/Teleport",
            "a.v1.Hosts/List",
            "b.v1.Hosts/List",
            "a.v1.Hosts/Teleport",
            "a.v1.Releases/List",
        ]),
        &set,
    )
    .expect_err("three calls are unknown");
    let calls: Vec<&str> = unknown.iter().map(|u| u.call.as_str()).collect();
    assert_eq!(
        calls,
        [
            "a.v1.Hosts/Teleport",
            "b.v1.Hosts/List",
            "a.v1.Releases/List"
        ]
    );
    assert_eq!(
        unknown[0].to_string(),
        "a.v1.Hosts/Teleport is not a method of the protobuf descriptors"
    );
}

#[test]
fn check_calls_refuses_every_call_against_an_empty_descriptor_set() {
    let empty = FileDescriptorSet::default();
    assert_eq!(
        check_calls(&names(&["a.v1.Hosts/List"]), &empty),
        Err(vec![UnknownCall {
            call: "a.v1.Hosts/List".to_owned()
        }])
    );
}

#[test]
fn call_sites_reads_each_literal_with_its_line_and_page() {
    let text = "// one call per button\nconst r = await ctx.api.call(\"hostkeeper.a.v1.Hosts/List\", {});\n\
                api.call(\n  \"hostkeeper.a.v1.Hosts/Get\",\n  { nodeId });\nrecall(x); callback(y); calls;\n";
    let sites = call_sites("pages/fleet.js", text).expect("the calls are literals");
    assert_eq!(
        sites,
        vec![
            CallSite {
                path: "pages/fleet.js".to_owned(),
                line: 2,
                method: "hostkeeper.a.v1.Hosts/List".to_owned()
            },
            CallSite {
                path: "pages/fleet.js".to_owned(),
                line: 4,
                method: "hostkeeper.a.v1.Hosts/Get".to_owned()
            },
        ]
    );
    assert_eq!(sites[0].page(), Some("fleet"));
}

#[test]
fn a_call_site_outside_the_pages_has_no_page() {
    let site = |path: &str| CallSite {
        path: path.to_owned(),
        line: 1,
        method: String::new(),
    };
    assert_eq!(site("app.js").page(), None);
    assert_eq!(site("pages/fleet.mjs").page(), None);
    assert_eq!(site("pages/holds.js").page(), Some("holds"));
}

#[test]
fn a_computed_or_escaped_method_name_is_refused_with_its_line() {
    for (text, line) in [
        ("\napi.call(name, {});", 2),
        ("api.call('hostkeeper.a.v1.Hosts/List');", 1),
        ("api.call(`hostkeeper.a.v1.Hosts/List`);", 1),
        ("\n\napi.call(\"hostkeeper.a.v1.Hosts/Li\\u0073t\");", 3),
        ("api.call(\"hostkeeper.a.v1.Hosts/List", 1),
        ("api.call(\"hostkeeper.a.v1.\nHosts/List\")", 1),
        ("call  (  prefix + \"Hosts/List\")", 1),
    ] {
        assert_eq!(
            call_sites("pages/x.js", text),
            Err(ScanError::NotALiteral {
                path: "pages/x.js".to_owned(),
                line
            }),
            "{text:?}"
        );
    }
}

#[test]
fn a_method_name_outside_a_call_is_refused() {
    let text =
        "api.call(\"hostkeeper.a.v1.Hosts/List\");\nconst m = \"hostkeeper.a.v1.Hosts/Get\";\n";
    assert_eq!(
        call_sites("pages/x.js", text),
        Err(ScanError::NameOutsideCall {
            path: "pages/x.js".to_owned(),
            line: 2
        })
    );
    assert_eq!(
        ScanError::NameOutsideCall {
            path: "pages/x.js".to_owned(),
            line: 2
        }
        .to_string(),
        "pages/x.js:2: a method name outside a call"
    );
    assert_eq!(
        ScanError::NotALiteral {
            path: "pages/x.js".to_owned(),
            line: 3
        }
        .to_string(),
        "pages/x.js:3: a call names its method by something other than a string literal"
    );
}

#[test]
fn calls_json_is_read_strictly() {
    let listed = listed_calls(r#"{"calls": [{"method": "a.v1.Hosts/List", "pages": ["fleet"]}]}"#)
        .expect("a well-formed list");
    assert_eq!(
        listed,
        vec![Listed {
            method: "a.v1.Hosts/List".to_owned(),
            pages: vec!["fleet".to_owned()]
        }]
    );
    let refused = listed_calls(r#"{"calls": [], "extra": 1}"#).expect_err("an unknown key");
    assert!(
        refused
            .to_string()
            .starts_with("ui/calls.json does not read: unknown field `extra`"),
        "{refused}"
    );
    assert!(listed_calls(r#"{"calls": [{"method": "m", "pages": [], "page": "x"}]}"#).is_err());
    assert!(listed_calls("[]").is_err());
}
