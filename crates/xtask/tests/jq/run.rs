//! A jq program run with jaq, with no shell and no jq binary, by the tests of the jq programs the
//! repository holds: the two programs of `set-desired.yml` (the workflow lint) and the queries of
//! the move runbook, `docs/operations/move-to-native.md` (the operator docs' test).

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{Compiler, Ctx, Vars, data};
use jaq_json::Val;

/// How a program ended after what it printed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ended {
    /// It ran to its end: jq exits 0.
    Done,
    /// `halt_error` with this exit code.
    Halted(i32),
    /// `error` with this message: jq exits 5.
    Failed(String),
}

/// The text of each value a program printed (jq's `--raw-output`), and how it ended.
pub(crate) type Ran = (Vec<String>, Ended);

/// Runs `program` on the JSON text `input` (`null` for jq's `--null-input`), its variables bound
/// to the texts of `vars`, as jq's `--arg` binds them.
pub(crate) fn run_on(program: &str, vars: &[(&str, String)], input: &str) -> Ran {
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs());
    let funs = jaq_core::funs()
        .chain(jaq_std::funs())
        .chain(jaq_json::funs());
    let arena = Arena::default();
    let modules = Loader::new(defs)
        .load(
            &arena,
            File {
                code: program,
                path: (),
            },
        )
        .unwrap_or_else(|e| panic!("the program parses: {e:?}"));
    let names: Vec<String> = vars.iter().map(|(name, _)| format!("${name}")).collect();
    let filter = Compiler::default()
        .with_funs(funs)
        .with_global_vars(names.iter().map(String::as_str))
        .compile(modules)
        .unwrap_or_else(|e| panic!("the program compiles: {e:?}"));
    let input = jaq_json::read::parse_single(input.as_bytes())
        .unwrap_or_else(|e| panic!("the input is one JSON value: {e:?}"));
    let values = vars.iter().map(|(_, value)| Val::from(value.clone()));
    let ctx = Ctx::<data::JustLut<Val>>::new(&filter.lut, Vars::new(values));
    let mut printed = Vec::new();
    for value in filter.id.run((ctx, input)) {
        match value {
            Ok(v) => {
                let json = v.to_string();
                printed.push(serde_json::from_str::<String>(&json).unwrap_or(json));
            }
            Err(e) => match e.get_halt() {
                Ok(code) => return (printed, Ended::Halted(code)),
                Err(e) => {
                    let error = e
                        .get_err()
                        .unwrap_or_else(|e| panic!("not an error: {e:?}"));
                    let message = error.to_string();
                    let message = serde_json::from_str::<String>(&message).unwrap_or(message);
                    return (printed, Ended::Failed(message));
                }
            },
        }
    }
    (printed, Ended::Done)
}
