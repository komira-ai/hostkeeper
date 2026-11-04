//! What the crate exports after the declaration machinery was deleted.

/// The deleted items, by the names they were exported under.
const DELETED: [&str; 6] = [
    "approve",
    "gate",
    "widening",
    "Approvals",
    "load_dir",
    "DECLARATION_DIR",
];

/// The identifiers of the code lines of `text` (comments left out).
fn identifiers(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .flat_map(|line| {
            line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .filter(|word| !word.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The deleted names that appear as identifiers in `text`.
fn reappearing(text: &str) -> Vec<&'static str> {
    let found = identifiers(text);
    DELETED
        .into_iter()
        .filter(|name| found.iter().any(|word| word == name))
        .collect()
}

/// Catches the declaration loader, the gate, the widening check or the approvals coming back
/// into the crate's exports: none of their names is a module or a re-export of `src/lib.rs`.
/// The crate's documentation may still say they were deleted.
#[test]
fn rights_crate_exports_no_declaration_gate_or_approval_items() {
    let lib = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
        .expect("read src/lib.rs");
    assert_eq!(reappearing(&lib), Vec::<&str>::new());
}

/// The check itself refuses each name as a module and as a re-export, and leaves a comment and
/// a longer identifier alone.
#[test]
fn the_export_check_finds_each_deleted_name() {
    for name in DELETED {
        assert_eq!(reappearing(&format!("pub mod {name};\n")), [name]);
        assert_eq!(reappearing(&format!("pub use x::{{a, {name}}};\n")), [name]);
        assert_eq!(
            reappearing(&format!("//! the {name} is gone\n")),
            Vec::<&str>::new()
        );
        assert_eq!(
            reappearing(&format!("pub use {name}_x;\n")),
            Vec::<&str>::new()
        );
    }
}
