//! The ELF reader on files crafted byte by byte: each shape it accepts and each defect it names.
//!
//! The 64-bit little-endian glibc program of [`Spec::glibc`] is laid out as: the header (64
//! bytes), three program headers of 56 bytes (`PT_LOAD` at 64, `PT_INTERP` at 120, `PT_DYNAMIC`
//! at 176), the interpreter at 232, the string table at 260 and the dynamic table at 271.

use super::fixture::{AARCH64, BASE, Spec, X86_64};
use super::*;

const INTERP: &str = "/lib64/ld-linux-x86-64.so.2";
const LOAD_AT: usize = 64;
const INTERP_AT: usize = 120;
const DYNAMIC_AT: usize = 176;
const STRTAB_AT: u64 = 260;
const TABLE_AT: usize = 271;

fn read(spec: &Spec) -> Elf {
    parse(&spec.build()).expect("an ELF the reader takes")
}

/// Overwrites `bytes` at `at` with `value`, little-endian, `width` bytes.
fn poke(bytes: &mut [u8], at: usize, value: u64, width: usize) {
    bytes[at..at + width].copy_from_slice(&value.to_le_bytes()[..width]);
}

fn poked(at: usize, value: u64, width: usize) -> Vec<u8> {
    let mut bytes = Spec::glibc().build();
    poke(&mut bytes, at, value, width);
    bytes
}

#[test]
fn a_static_pie_has_a_dynamic_segment_but_no_needed_entry_and_no_interpreter() {
    assert_eq!(
        read(&Spec::static_pie(X86_64)),
        Elf {
            class: 2,
            little_endian: true,
            machine: X86_64,
            interpreter: None,
            needed: Vec::new(),
        }
    );
    assert_eq!(read(&Spec::static_pie(AARCH64)).machine, AARCH64);
}

#[test]
fn a_fully_static_program_has_no_dynamic_segment() {
    let spec = Spec {
        dynamic: false,
        ..Spec::static_pie(X86_64)
    };
    let elf = read(&spec);
    assert_eq!((elf.interpreter, elf.needed), (None, Vec::new()));
}

#[test]
fn a_program_with_no_program_headers_is_static() {
    let mut bytes = Spec::glibc().build();
    poke(&mut bytes, 56, 0, 2);
    let elf = parse(&bytes).expect("parsed");
    assert_eq!((elf.interpreter, elf.needed), (None, Vec::new()));
}

#[test]
fn the_interpreter_and_every_needed_name_are_read_in_each_class_and_byte_order() {
    for (class, big_endian) in [(2, false), (1, false), (2, true), (1, true)] {
        let spec = Spec {
            class,
            big_endian,
            needed: vec!["libc.so.6", "libssl.so.3"],
            ..Spec::glibc()
        };
        let elf = read(&spec);
        assert_eq!(elf.class, class);
        assert_eq!(elf.little_endian, !big_endian);
        assert_eq!(elf.machine, X86_64);
        assert_eq!(elf.interpreter.as_deref(), Some(INTERP));
        assert_eq!(elf.needed, ["libc.so.6", "libssl.so.3"]);
    }
}

#[test]
fn an_interpreter_whose_segment_holds_no_nul_runs_to_the_end_of_the_segment() {
    let bytes = poked(INTERP_AT + 32, INTERP.len() as u64, 8);
    assert_eq!(
        parse(&bytes).expect("parsed").interpreter.as_deref(),
        Some(INTERP)
    );
}

#[test]
fn a_dynamic_table_without_a_terminator_ends_with_its_segment() {
    let mut bytes = Spec::glibc().build();
    let end = bytes.len();
    bytes[end - 16..].fill(0xff);
    assert_eq!(parse(&bytes).expect("parsed").needed, ["libc.so.6"]);
}

#[test]
fn each_defect_of_an_elf_file_is_named() {
    let good = Spec::glibc().build();
    let name_value = TABLE_AT + 8;
    let mut unterminated = good.clone();
    let end = unterminated.len();
    unterminated[end - 16..].fill(0xff);
    poke(
        &mut unterminated,
        name_value,
        (end - 16) as u64 - STRTAB_AT,
        8,
    );
    let moved = BASE + 0x10_0000;
    let mut tag_gone = good.clone();
    poke(&mut tag_gone, TABLE_AT + 16, 0x6fff_fef5, 8);
    let huge = 1u64 << 40;
    let cases: Vec<(Vec<u8>, String)> = vec![
        (b"#!/bin/sh\n".to_vec(), "no ELF magic".into()),
        (
            b"\x7fELF\x02\x01".to_vec(),
            "6 bytes, too short for an ELF header".into(),
        ),
        (
            poked(4, 3, 1),
            "ELF class 3, not 1 (32-bit) or 2 (64-bit)".into(),
        ),
        (
            poked(5, 3, 1),
            "ELF data 3, not 1 (little-endian) or 2 (big-endian)".into(),
        ),
        (
            good[..18].to_vec(),
            "e_machine (2 bytes at offset 18) is outside the file".into(),
        ),
        (
            good[..34].to_vec(),
            "e_phoff (8 bytes at offset 32) is outside the file".into(),
        ),
        (
            poked(54, 8, 2),
            "program headers of 8 bytes, fewer than the 56 of this class".into(),
        ),
        (
            poked(32, huge, 8),
            format!("a program header (offset {huge}) is outside the file"),
        ),
        (
            good[..INTERP_AT + 20].to_vec(),
            "a program header (8 bytes at offset 136) is outside the file".into(),
        ),
        (
            poked(INTERP_AT + 8, huge, 8),
            format!("PT_INTERP (offset {huge}) is outside the file"),
        ),
        (
            poked(INTERP_AT + 32, huge, 8),
            format!("PT_INTERP ({huge} bytes at offset 232) is outside the file"),
        ),
        (
            poked(DYNAMIC_AT + 8, huge, 8),
            format!("PT_DYNAMIC (offset {huge}) is outside the file"),
        ),
        (tag_gone, "DT_NEEDED without DT_STRTAB".into()),
        (
            poked(name_value, huge, 8),
            format!(
                "a DT_NEEDED name (offset {}) is outside the file",
                huge + STRTAB_AT
            ),
        ),
        (
            unterminated,
            "a DT_NEEDED name has no end inside the file".into(),
        ),
        (
            poked(LOAD_AT + 16, moved, 8),
            format!("DT_STRTAB {:#x} is in no PT_LOAD segment", BASE + STRTAB_AT),
        ),
        (
            poked(LOAD_AT + 32, 100, 8),
            format!("DT_STRTAB {:#x} is in no PT_LOAD segment", BASE + STRTAB_AT),
        ),
    ];
    for (bytes, want) in cases {
        assert_eq!(parse(&bytes), Err(want));
    }
}

#[test]
fn a_32_bit_file_with_short_program_headers_is_refused() {
    let spec = Spec {
        class: 1,
        ..Spec::glibc()
    };
    let mut bytes = spec.build();
    poke(&mut bytes, 42, 8, 2);
    assert_eq!(
        parse(&bytes),
        Err("program headers of 8 bytes, fewer than the 32 of this class".to_owned())
    );
}

#[test]
fn a_program_that_is_not_a_file_of_the_elf_class_is_told_from_one_that_is() {
    assert!(is_elf(&Spec::glibc().build()));
    assert!(!is_elf(b"#!/bin/sh"));
}
