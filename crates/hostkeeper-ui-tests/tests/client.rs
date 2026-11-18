//! The generated client under `ui/gen/` against what the generator makes now, its declarations
//! read by a TypeScript parser, the client run in the JavaScript engine against the fake
//! gateway, and `ui/calls.json` against what the pages call.
//!
//! When a checked-in file differs, the test writes the current text in its scratch directory
//! (under the target directory), prints where, and fails: copy it over the checked-in file.

mod support;

use std::collections::BTreeSet;
use std::path::Path;

use hostkeeper_proto::ctl::COMMANDS;
use hostkeeper_ui_tests::client_gen::{Function, current, functions};
use hostkeeper_ui_tests::{UI_FILES, UiFile, call_sites, calls_json, derived_calls};
use serde_json::{Value, json};
use support::Ui;
use swc_common::BytePos;
use swc_ecma_ast::{
    Decl, Expr, ImportSpecifier, ModuleDecl, ModuleItem, TsInterfaceDecl, TsType, TsTypeElement,
};
use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax};

/// Fails unless the checked-in `ui/<path>` is `current`; when it is not, writes `current` to
/// this test's scratch directory and says where.
fn assert_current(path: &str, current: &str) {
    let held = UiFile::named(path)
        .unwrap_or_else(|| panic!("ui/{path} is not a file of the app"))
        .text;
    if held != current {
        let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join(path);
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir).expect("make the scratch directory");
        }
        std::fs::write(&out, current).expect("write the current text");
        panic!(
            "ui/{path} is not what the protos and the pages make; the current text is in {}",
            out.display()
        );
    }
}

/// The `.d.ts` files the app holds under `gen/`, by name.
fn held_declarations() -> BTreeSet<String> {
    UI_FILES
        .iter()
        .filter_map(|f| f.path.strip_prefix("gen/"))
        .filter(|name| name.ends_with(".d.ts"))
        .map(str::to_owned)
        .collect()
}

/// Proves: `ui/gen/client.js` and every declarations file under `ui/gen/` are what the
/// generator makes now, and the declarations files the app holds are exactly the emitted ones.
/// Catches: `ui/gen/` left behind when a proto, the command table or the generator changes, a hand
/// edit of a file, and a stale `.d.ts` (a package that is gone, a file nothing emits) left beside
/// the current ones; also a `.d.ts` on disk that the crate's file list misses.
#[test]
fn ui_gen_equals_the_emitters_output() {
    let client = current().expect("the client generates");
    assert_current("gen/client.js", &client.js);
    for (name, text) in &client.dts {
        assert_current(&format!("gen/{name}"), text);
    }
    let emitted: BTreeSet<String> = client.dts.iter().map(|(n, _)| n.clone()).collect();
    assert_eq!(held_declarations(), emitted, "the held gen/*.d.ts files");
    let mut on_disk = BTreeSet::new();
    let gen_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/gen");
    for entry in std::fs::read_dir(&gen_dir).expect("read ui/gen") {
        let name = entry.expect("a directory entry").file_name();
        let name = name.to_string_lossy().into_owned();
        if name.ends_with(".d.ts") {
            on_disk.insert(name);
        }
    }
    assert_eq!(on_disk, emitted, "the gen/*.d.ts files on disk");
}

/// The module `text` is, read as TypeScript declarations (`.d.ts`): the parser's errors, the
/// fatal one first, as their codes; or the module.
fn declarations(text: &str) -> Result<swc_ecma_ast::Module, Vec<String>> {
    let syntax = Syntax::Typescript(TsSyntax {
        dts: true,
        ..TsSyntax::default()
    });
    let end = u32::try_from(text.len()).expect("a short text") + 1;
    let mut parser = Parser::new(
        syntax,
        StringInput::new(text, BytePos(1), BytePos(end)),
        None,
    );
    let parsed = parser.parse_module();
    let mut errors: Vec<String> = Vec::new();
    if let Err(fatal) = &parsed {
        errors.push(format!("{:?}", fatal.kind()));
    }
    errors.extend(
        parser
            .take_errors()
            .iter()
            .map(|e| format!("{:?}", e.kind())),
    );
    match parsed {
        Ok(module) if errors.is_empty() => Ok(module),
        _ => Err(errors),
    }
}

/// Whether `import` is `import type { X } from "./<name>.js"`: named, type-only, from a sibling
/// file of the same directory whose name ends in `.js` (the declarations of a generated file).
fn is_sibling_type_import(import: &swc_ecma_ast::ImportDecl) -> bool {
    let path = import.src.value.to_string_lossy();
    let sibling = path
        .strip_prefix("./")
        .and_then(|name| name.strip_suffix(".js"))
        .is_some_and(|name| {
            !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
        });
    import.type_only
        && import.with.is_none()
        && sibling
        && !import.specifiers.is_empty()
        && import.specifiers.iter().all(
            |s| matches!(s, ImportSpecifier::Named(n) if !n.is_type_only && n.imported.is_none()),
        )
}

/// What a declarations file may hold at its top level: `export` of an interface, a type, a
/// namespace, or a function with no body, and `import type { X } from "./<name>.js"` of a
/// sibling generated file. Anything else, a statement that runs, a default export, any other
/// import, is named.
fn top_level(module: &swc_ecma_ast::Module) -> Result<(), String> {
    for item in &module.body {
        let declared = match item {
            ModuleItem::ModuleDecl(ModuleDecl::Import(import)) => is_sibling_type_import(import),
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => match &export.decl {
                Decl::TsInterface(_) | Decl::TsTypeAlias(_) | Decl::TsModule(_) => true,
                Decl::Fn(f) => f.function.body.is_none(),
                _ => false,
            },
            _ => false,
        };
        if !declared {
            return Err(format!("{item:?}").chars().take(60).collect());
        }
    }
    Ok(())
}

/// The name of a property or method key that is a plain name.
fn key(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(id) => Some(id.sym.to_string()),
        _ => None,
    }
}

/// The functions `Client` declares, as `<service>.<function>`.
fn declared_functions(client: &TsInterfaceDecl) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for member in &client.body.body {
        let TsTypeElement::TsPropertySignature(service) = member else {
            panic!("Client has a member that is not a service: {member:?}");
        };
        let name = key(&service.key).expect("a service is a plain name");
        assert!(service.readonly, "{name} is not readonly");
        let Some(TsType::TsTypeLit(object)) = service.type_ann.as_ref().map(|t| &*t.type_ann)
        else {
            panic!("{name} is not an object of functions");
        };
        for function in &object.members {
            let TsTypeElement::TsMethodSignature(method) = function else {
                panic!("{name} has a member that is not a function: {function:?}");
            };
            out.insert(format!(
                "{name}.{}",
                key(&method.key).expect("a function is a plain name")
            ));
        }
    }
    out
}

/// The `Client` interface of a declarations module.
fn client_interface(module: &swc_ecma_ast::Module) -> &TsInterfaceDecl {
    module
        .body
        .iter()
        .find_map(|item| match item {
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => match &export.decl {
                Decl::TsInterface(i) if &*i.id.sym == "Client" => Some(&**i),
                _ => None,
            },
            _ => None,
        })
        .expect("the declarations export Client")
}

/// Every function of the generated client, as `<service>.<function>`.
fn table_functions() -> BTreeSet<String> {
    functions(COMMANDS).iter().map(Function::call).collect()
}

/// Proves: every `.d.ts` under `ui/gen/` parses as TypeScript declarations holding only
/// declarations and sibling type imports, each import names a namespace file that exists, and
/// `client.d.ts`'s `Client` has exactly the table's functions.
/// Catches: a declarations file a TypeScript parser refuses, one that holds code rather than
/// declarations, an import of a file that is not generated, and a `Client` that misses a function
/// of the table or adds one.
#[test]
fn client_d_ts_parses() {
    let names = held_declarations();
    assert!(names.contains("client.d.ts"), "{names:?}");
    for name in &names {
        let text = UiFile::named(&format!("gen/{name}"))
            .expect("a file of the app")
            .text;
        let module = declarations(text).unwrap_or_else(|e| panic!("{name} does not parse: {e:?}"));
        assert_eq!(top_level(&module), Ok(()), "{name}");
        for item in &module.body {
            if let ModuleItem::ModuleDecl(ModuleDecl::Import(import)) = item {
                let target = import.src.value.to_string_lossy().into_owned();
                let file = target.trim_start_matches("./").replace(".js", ".d.ts");
                assert!(
                    names.contains(&file),
                    "{name} imports {target}: no such file"
                );
            }
        }
    }
    let text = UiFile::named("gen/client.d.ts")
        .expect("ui/gen/client.d.ts is a file of the app")
        .text;
    let module = declarations(text).expect("client.d.ts parses");
    let declared = declared_functions(client_interface(&module));
    assert_eq!(declared, table_functions());
    assert!(declared.contains("releases.setDesired"), "{declared:?}");
}

/// Proves: the parse check admits declarations and `import type { X } from "./<name>.js"` of a
/// sibling file, and names every other import, statement and default export.
/// Catches: a parse check that passes what a TypeScript compiler would refuse in a `.d.ts` or a
/// generated file may not hold: a syntax error, a function with a body, a statement that runs, a
/// default export, a type left unexported, a value import, an import of a file outside the
/// directory, an import that is not type-only or not named.
#[test]
fn the_declarations_check_refuses_what_a_d_ts_may_not_hold() {
    let good = "import type { x_y } from \"./x_y.js\";\nimport type { p, q } from \"./p.js\";\n\
                export interface A { b?: string; }\nexport type C = \"X\" | \"Y\";\n\
                export namespace d.e { export interface F { g?: d.e.F[]; } }\n\
                export declare function h(i: A): Promise<C>;\n";
    let module = declarations(good).expect("the declarations parse");
    assert_eq!(top_level(&module), Ok(()));

    let broken = declarations("export interface A { b?: ; }").map(|_| ());
    assert!(broken.as_ref().is_err_and(|e| !e.is_empty()), "{broken:?}");
    let body = declarations("export function f(): number { return 1; }");
    assert_eq!(body.map(|_| ()), Err(vec!["TS1183".to_owned()]));
    for code in [
        "const y = 1;",
        "export const y: number;",
        "export default interface A {}",
        "interface A {}",
        "import { a } from \"./b.js\";",
        "import type { a } from \"./b\";",
        "import type { a } from \"../b.js\";",
        "import type { a } from \"./sub/b.js\";",
        "import type { a } from \"/b.js\";",
        "import type { a } from \"b.js\";",
        "import type { a } from \"https://example.test/b.js\";",
        "import type a from \"./b.js\";",
        "import type * as a from \"./b.js\";",
        "import type {} from \"./b.js\";",
        "import type { a as c } from \"./b.js\";",
        "import \"./b.js\";",
        "import a = require(\"./b.js\");",
    ] {
        let module = declarations(code).unwrap_or_else(|e| panic!("{code}: {e:?}"));
        assert!(top_level(&module).is_err(), "{code}");
    }
}

/// Catches a generated function that posts to another method than its own, sends another body,
/// or reaches the network past the data layer; and a client whose objects a page could change.
#[test]
fn every_generated_function_posts_to_its_method_through_the_data_layer() {
    let all = functions(COMMANDS);
    let calls: Vec<String> = all
        .iter()
        .map(|f| {
            format!(
                "  try {{ await client.{}({{}}); }} catch (e) {{ codes.push(e.code); }}",
                f.call()
            )
        })
        .collect();
    let mut ui = Ui::start();
    let out = ui.run(&format!(
        r#"
import {{ api }} from "../test/kit.js";
export async function main() {{
  const client = api("tok_client_1");
  const codes = [];
{}
  const frozen = Object.isFrozen(client) && Object.values(client).every(Object.isFrozen);
  const shape = Object.keys(client).map((s) => Object.keys(client[s]).map((f) => `${{s}}.${{f}}`)).flat();
  return {{ codes, frozen, shape }};
}}"#,
        calls.join("\n")
    ));
    let methods: Vec<String> = ui
        .requests()
        .iter()
        .map(|r| {
            assert_eq!(r.http_method(), "POST");
            assert_eq!(r.header("x-hk-csrf"), Some("tok_client_1"));
            assert_eq!(r.header("authorization"), None);
            assert_eq!(r.body(), json!({}));
            r.method().to_owned()
        })
        .collect();
    let expected: Vec<String> = all.iter().map(|f| f.method.clone()).collect();
    assert_eq!(methods, expected);
    // No reply was scripted: each call is the gateway's answer for an unmerged handler.
    assert_eq!(out["codes"], json!(vec!["UNIMPLEMENTED"; all.len()]));
    assert_eq!(out["frozen"], true);
    let shape: Vec<String> = out["shape"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|v| v.as_str().expect("a name").to_owned())
        .collect();
    let expected: Vec<String> = all.iter().map(Function::call).collect();
    assert_eq!(shape, expected);
}

/// Catches a client whose page-facing object still offers the raw `call`, a second way to call
/// a method that the list of calls would not see.
#[test]
fn the_client_the_pages_get_has_no_raw_call() {
    let mut ui = Ui::start();
    let out = ui.run(
        r#"
import { api } from "../test/kit.js";
export async function main() {
  const client = api("tok_client_2");
  return [typeof client["call"], "call" in client, Object.keys(client).includes("call")];
}"#,
    );
    assert_eq!(out, json!(["undefined", false, false]));
}

/// Catches a `ui/calls.json` edited by hand or left behind when a page adds, drops or moves a
/// call: the list is what the scan of the pages derives, byte for byte.
#[test]
fn calls_json_is_derived_from_the_pages() {
    let mut sites = Vec::new();
    for file in UI_FILES
        .iter()
        .filter(|f| f.is_script() && !["api.js", "gen/client.js"].contains(&f.path))
    {
        sites.extend(call_sites(file.path, file.text).unwrap_or_else(|e| panic!("{e}")));
    }
    let entries = derived_calls(&sites, &functions(COMMANDS)).unwrap_or_else(|e| panic!("{e}"));
    assert!(entries.len() > 1, "the pages call: {entries:?}");
    assert_current("calls.json", &calls_json(&entries));
}

/// The pages' calls, the parse of `calls.json` and the derived list agree on one entry: a
/// regression check of the JSON a reader sees.
#[test]
fn calls_json_names_each_function_with_its_method_and_pages() {
    let held = UiFile::named("calls.json").expect("calls.json").text;
    let value: Value = serde_json::from_str(held).expect("calls.json is JSON");
    let entry = value["calls"]
        .as_array()
        .expect("a list of calls")
        .iter()
        .find(|c| c["function"] == "releases.setDesired")
        .expect("the Rollout page sets the desired version");
    assert_eq!(
        entry,
        &json!({"function": "releases.setDesired",
                "method": "hostkeeper.operator.v1.Releases/SetDesired",
                "pages": ["rollout"]})
    );
}
