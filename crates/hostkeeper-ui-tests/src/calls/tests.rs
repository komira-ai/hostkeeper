use prost_types::{FileDescriptorProto, MethodDescriptorProto, ServiceDescriptorProto};

use super::*;
use crate::client_gen::Function;

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

fn site(path: &str, line: usize, function: &str) -> CallSite {
    CallSite {
        path: path.to_owned(),
        line,
        function: function.to_owned(),
    }
}

#[test]
fn call_sites_reads_each_call_with_its_line_and_page() {
    // Catches: a call missed, read with the wrong line or function, and a longer name, the
    // module name `api.js` or the client passed whole taken as a call.
    let text = "// one call per button, through `api.js`\nimport { createApi } from \"./api.js\";\n\
                const r = await ctx.api.hosts.list({});\n\
                api.releases\n  .setDesired(\n  { nodeId });\n\
                myapi.x.y(); $api.x.y(); api_x.y.z(); apix; listHosts(ctx.api); f(api, 1);\n\
                api.js.list(); api.jsx.list();\n";
    let sites = call_sites("pages/fleet.js", text).expect("the calls are read");
    assert_eq!(
        sites,
        vec![
            site("pages/fleet.js", 3, "hosts.list"),
            site("pages/fleet.js", 4, "releases.setDesired"),
            site("pages/fleet.js", 8, "js.list"),
            site("pages/fleet.js", 8, "jsx.list"),
        ]
    );
    assert_eq!(sites[0].page(), Some("fleet"));
}

#[test]
fn blanks_between_the_client_its_names_and_its_parenthesis_are_read_through() {
    // Catches: a call hidden from the list by a comment or a line break around its dots or
    // before its `(`.
    let text = "api /* why */ . hosts // the service\n  . list /* x */ (\n  {});\n\
                ctx.api.hosts.get\n({ nodeId });";
    let functions: Vec<String> = call_sites("pages/x.js", text)
        .expect("both are calls")
        .into_iter()
        .map(|s| s.function)
        .collect();
    assert_eq!(functions, ["hosts.list", "hosts.get"]);
}

#[test]
fn a_call_site_outside_the_pages_has_no_page() {
    assert_eq!(site("app.js", 1, "").page(), None);
    assert_eq!(site("pages/fleet.mjs", 1, "").page(), None);
    assert_eq!(site("pages/holds.js", 1, "").page(), Some("holds"));
}

#[test]
fn a_call_not_written_as_a_function_of_the_client_is_refused_with_its_line() {
    // Catches: a call the list would not see: the raw `call`, a computed service or function,
    // a function taken out of the client, called through `call`/`apply`, or a client cut short.
    for (text, line) in [
        ("\napi.call(name, {});", 2),
        ("api[\"hosts\"].list({});", 1),
        ("api .  [\"hosts\"];", 1),
        ("ctx.api.hosts[\"list\"]({});", 1),
        ("\n\nconst f = ctx.api.hosts.list;", 3),
        ("api.hosts.list.call(null, {});", 1),
        ("api.hosts.list.apply(null, [{}]);", 1),
        ("const h = api.hosts;", 1),
        ("api.hosts.list", 1),
        ("api.", 1),
        ("api.hosts.", 1),
        ("api.hosts.list`x`", 1),
        ("api.js()", 1),
        ("ctx.api?.hosts.list({});", 1),
        ("api ?.hosts.list({});", 1),
        ("\nctx[\"api\"].hosts.list({});", 2),
        ("ctx['api'].hosts.list({});", 1),
    ] {
        assert_eq!(
            call_sites("pages/x.js", text),
            Err(ScanError::NotACall {
                path: "pages/x.js".to_owned(),
                line
            }),
            "{text:?}"
        );
    }
}

#[test]
fn the_client_given_another_name_is_refused() {
    // Catches: a call hidden behind another name for the client or one of its services.
    for (text, line) in [
        ("const a = ctx.api;", 1),
        ("\nlet { hosts } = api;", 2),
        ("x =api", 1),
        ("x = ctx . page . api", 1),
    ] {
        assert_eq!(
            call_sites("pages/x.js", text),
            Err(ScanError::Aliased {
                path: "pages/x.js".to_owned(),
                line
            }),
            "{text:?}"
        );
    }
    for text in [
        "if (x == api) {}",
        "if (x === ctx.api) {}",
        "if (x != api) {}",
        "if (x <= api || x >= api) {}",
        "const f = () => api;",
        "const api = createApi(deps);",
    ] {
        assert_eq!(call_sites("pages/x.js", text), Ok(Vec::new()), "{text:?}");
    }
}

#[test]
fn a_method_name_in_a_page_is_refused() {
    // Catches: a page that spells a method itself, which only the generated client does.
    let text = "api.hosts.list({});\nconst m = \"hostkeeper.a.v1.Hosts/Get\";\n";
    assert_eq!(
        call_sites("pages/x.js", text),
        Err(ScanError::MethodName {
            path: "pages/x.js".to_owned(),
            line: 2
        })
    );
    for (error, text) in [
        (
            ScanError::MethodName {
                path: "pages/x.js".to_owned(),
                line: 2,
            },
            "pages/x.js:2: a method name outside the generated client",
        ),
        (
            ScanError::NotACall {
                path: "pages/x.js".to_owned(),
                line: 3,
            },
            "pages/x.js:3: `api.` is not a call of `api.<service>.<function>(...)`",
        ),
        (
            ScanError::Aliased {
                path: "pages/x.js".to_owned(),
                line: 4,
            },
            "pages/x.js:4: the client is given another name",
        ),
    ] {
        assert_eq!(error.to_string(), text);
    }
}

fn function(service: &str, name: &str, method: &str) -> Function {
    Function {
        service: service.to_owned(),
        name: name.to_owned(),
        method: method.to_owned(),
    }
}

fn listed(function: &str, method: &str, pages: &[&str]) -> Listed {
    Listed {
        function: function.to_owned(),
        method: method.to_owned(),
        pages: pages.iter().map(|p| (*p).to_owned()).collect(),
    }
}

#[test]
fn derived_calls_lists_each_function_by_method_with_its_pages_once() {
    // Catches: a list out of method order, a page listed twice or out of order, and a function
    // listed under another's method.
    let functions = [
        function("hosts", "list", "a.v1.Hosts/List"),
        function("hosts", "get", "a.v1.Hosts/Get"),
        function("alerts", "list", "a.v1.Alerts/List"),
    ];
    let sites = [
        site("pages/rollout.js", 1, "hosts.list"),
        site("pages/fleet.js", 1, "hosts.list"),
        site("pages/rollout.js", 9, "hosts.list"),
        site("pages/host.js", 2, "hosts.get"),
        site("pages/fleet.js", 3, "alerts.list"),
    ];
    let entries = derived_calls(&sites, &functions).expect("every call is a function");
    assert_eq!(
        entries,
        vec![
            listed("alerts.list", "a.v1.Alerts/List", &["fleet"]),
            listed("hosts.get", "a.v1.Hosts/Get", &["host"]),
            listed("hosts.list", "a.v1.Hosts/List", &["fleet", "rollout"]),
        ]
    );
    let text = calls_json(&entries);
    assert_eq!(
        text,
        "{\n  \"calls\": [\n\
         \x20   { \"function\": \"alerts.list\", \"method\": \"a.v1.Alerts/List\", \"pages\": [\"fleet\"] },\n\
         \x20   { \"function\": \"hosts.get\", \"method\": \"a.v1.Hosts/Get\", \"pages\": [\"host\"] },\n\
         \x20   { \"function\": \"hosts.list\", \"method\": \"a.v1.Hosts/List\", \"pages\": [\"fleet\", \"rollout\"] }\n\
         \x20 ]\n}\n"
    );
    assert_eq!(listed_calls(&text).expect("the text reads back"), entries);
    assert_eq!(derived_calls(&[], &functions), Ok(Vec::new()));
}

#[test]
fn derived_calls_refuses_an_unknown_function_and_a_call_outside_the_pages() {
    let functions = [function("hosts", "list", "a.v1.Hosts/List")];
    let unknown = derived_calls(
        &[
            site("pages/fleet.js", 1, "hosts.list"),
            site("pages/fleet.js", 4, "hosts.teleport"),
        ],
        &functions,
    );
    assert_eq!(
        unknown,
        Err(DeriveError::UnknownFunction {
            path: "pages/fleet.js".to_owned(),
            line: 4,
            function: "hosts.teleport".to_owned()
        })
    );
    assert_eq!(
        unknown.expect_err("unknown").to_string(),
        "pages/fleet.js:4: api.hosts.teleport is not a function of the generated client"
    );
    let outside = derived_calls(&[site("app.js", 7, "hosts.list")], &functions);
    assert_eq!(
        outside,
        Err(DeriveError::OutsidePages {
            path: "app.js".to_owned(),
            line: 7
        })
    );
    assert_eq!(
        outside.expect_err("outside").to_string(),
        "app.js:7: a call outside pages/"
    );
}

#[test]
fn calls_json_is_read_strictly() {
    let read = listed_calls(
        r#"{"calls": [{"function": "hosts.list", "method": "a.v1.Hosts/List", "pages": ["fleet"]}]}"#,
    )
    .expect("a well-formed list");
    assert_eq!(
        read,
        vec![listed("hosts.list", "a.v1.Hosts/List", &["fleet"])]
    );
    let refused = listed_calls(r#"{"calls": [], "extra": 1}"#).expect_err("an unknown key");
    assert!(
        refused
            .to_string()
            .starts_with("ui/calls.json does not read: unknown field `extra`"),
        "{refused}"
    );
    assert!(
        listed_calls(r#"{"calls": [{"function": "f", "method": "m", "pages": [], "page": "x"}]}"#)
            .is_err()
    );
    // The list before the generated client, with no function: a list left behind.
    assert!(listed_calls(r#"{"calls": [{"method": "m", "pages": []}]}"#).is_err());
    assert!(listed_calls("[]").is_err());
}
