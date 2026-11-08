//! ELF files crafted byte by byte for the tests of this crate: one `PT_LOAD` segment over the
//! whole file, and optionally a `PT_INTERP` and a `PT_DYNAMIC` with a string table.

/// The virtual address the `PT_LOAD` segment maps the file's first byte to.
pub(crate) const BASE: u64 = 0x1000;

/// `EM_X86_64`.
pub(crate) const X86_64: u16 = 0x3e;

/// `EM_AARCH64`.
pub(crate) const AARCH64: u16 = 0xb7;

/// What an ELF file is made of.
#[derive(Clone)]
pub(crate) struct Spec {
    /// 1 for 32-bit, 2 for 64-bit.
    pub(crate) class: u8,
    pub(crate) big_endian: bool,
    pub(crate) machine: u16,
    pub(crate) interpreter: Option<&'static str>,
    /// Whether the file has a `PT_DYNAMIC` segment.
    pub(crate) dynamic: bool,
    pub(crate) needed: Vec<&'static str>,
}

impl Spec {
    /// A 64-bit little-endian static-pie: a `PT_DYNAMIC` with no `DT_NEEDED`, no interpreter.
    pub(crate) fn static_pie(machine: u16) -> Spec {
        Spec {
            class: 2,
            big_endian: false,
            machine,
            interpreter: None,
            dynamic: true,
            needed: Vec::new(),
        }
    }

    /// A 64-bit little-endian x86-64 program linked to glibc.
    pub(crate) fn glibc() -> Spec {
        Spec {
            interpreter: Some("/lib64/ld-linux-x86-64.so.2"),
            needed: vec!["libc.so.6"],
            ..Spec::static_pie(X86_64)
        }
    }

    /// The program's bytes.
    pub(crate) fn build(&self) -> Vec<u8> {
        let wide = self.class == 2;
        let word: usize = if wide { 8 } else { 4 };
        let (ehsize, phentsize) = if wide { (64, 56) } else { (52, 32) };
        let phnum = 1 + usize::from(self.interpreter.is_some()) + usize::from(self.dynamic);
        let mut at = ehsize + phnum * phentsize;
        let interp = self.interpreter.map(|i| {
            let mut bytes = i.as_bytes().to_vec();
            bytes.push(0);
            bytes
        });
        let interp_at = at;
        at += interp.as_ref().map_or(0, Vec::len);
        let mut strtab = vec![0];
        let mut names = Vec::new();
        for name in &self.needed {
            names.push(strtab.len() as u64);
            strtab.extend(name.as_bytes());
            strtab.push(0);
        }
        let strtab_at = at;
        at += strtab.len();
        let dynamic_at = at;
        let mut entries: Vec<(u64, u64)> = names.iter().map(|n| (1, *n)).collect();
        entries.push((5, BASE + strtab_at as u64));
        entries.push((0, 0));
        let dynamic_len = entries.len() * 2 * word;
        let total = dynamic_at + dynamic_len;

        let mut out = Vec::new();
        let put = |out: &mut Vec<u8>, value: u64, width: usize| {
            let bytes = value.to_le_bytes();
            let mut part = bytes[..width].to_vec();
            if self.big_endian {
                part.reverse();
            }
            out.extend(part);
        };
        out.extend(b"\x7fELF");
        out.extend([self.class, if self.big_endian { 2 } else { 1 }, 1]);
        out.resize(16, 0);
        put(&mut out, 3, 2);
        put(&mut out, u64::from(self.machine), 2);
        put(&mut out, 1, 4);
        put(&mut out, 0, word);
        put(&mut out, ehsize as u64, word);
        put(&mut out, 0, word);
        put(&mut out, 0, 4);
        put(&mut out, ehsize as u64, 2);
        put(&mut out, phentsize as u64, 2);
        put(&mut out, phnum as u64, 2);
        put(&mut out, 0, 6);
        let segment = |out: &mut Vec<u8>, kind: u64, offset: usize, vaddr: u64, size: usize| {
            let (offset, size) = (offset as u64, size as u64);
            put(out, kind, 4);
            if wide {
                put(out, 5, 4);
            }
            put(out, offset, word);
            put(out, vaddr, word);
            put(out, vaddr, word);
            put(out, size, word);
            put(out, size, word);
            if !wide {
                put(out, 5, 4);
            }
            put(out, 8, word);
        };
        segment(&mut out, 1, 0, BASE, total);
        if let Some(interp) = &interp {
            segment(
                &mut out,
                3,
                interp_at,
                BASE + interp_at as u64,
                interp.len(),
            );
        }
        if self.dynamic {
            segment(
                &mut out,
                2,
                dynamic_at,
                BASE + dynamic_at as u64,
                dynamic_len,
            );
        }
        assert_eq!(out.len(), ehsize + phnum * phentsize);
        if let Some(interp) = &interp {
            out.extend(interp);
        }
        out.extend(&strtab);
        for (tag, value) in entries {
            put(&mut out, tag, word);
            put(&mut out, value, word);
        }
        assert_eq!(out.len(), total);
        out
    }
}
