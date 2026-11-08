//! What `release nfpm-stage` and `release check-linux-pkg` read of an ELF file: its machine, its
//! interpreter (`PT_INTERP`) and the libraries it needs (`DT_NEEDED`), the two things a static
//! program has none of. A static-pie program has a `PT_DYNAMIC` segment (it relocates itself)
//! but no `DT_NEEDED` entry and no `PT_INTERP`; a fully static one has no `PT_DYNAMIC` either.
//!
//! The reader takes the class (32 or 64 bit) and the byte order from the file's own `e_ident`,
//! walks the program headers, and finds each needed name through `DT_STRTAB`, a virtual address
//! that it maps to a file offset through the `PT_LOAD` segment holding it. Every offset and
//! length is checked: a file that is cut short or points outside itself is an error naming what
//! is wrong, never a program that passes and never a panic.

/// The ELF magic.
const MAGIC: &[u8; 4] = b"\x7fELF";

const PT_LOAD: u64 = 1;
const PT_DYNAMIC: u64 = 2;
const PT_INTERP: u64 = 3;
const DT_NULL: u64 = 0;
const DT_NEEDED: u64 = 1;
const DT_STRTAB: u64 = 5;

/// What an ELF file says of itself.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Elf {
    /// 1 for 32-bit, 2 for 64-bit.
    pub(crate) class: u8,
    /// Whether the file is little-endian.
    pub(crate) little_endian: bool,
    /// `e_machine`.
    pub(crate) machine: u16,
    /// The `PT_INTERP` string, when the file has one.
    pub(crate) interpreter: Option<String>,
    /// The name of each `DT_NEEDED` entry, in order.
    pub(crate) needed: Vec<String>,
}

/// Whether `bytes` start with the ELF magic.
pub(crate) fn is_elf(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// A segment of the program header table.
struct Segment {
    kind: u64,
    offset: u64,
    vaddr: u64,
    filesz: u64,
}

/// The bytes of a file read in its own byte order and word size.
struct Reader<'a> {
    bytes: &'a [u8],
    little: bool,
    wide: bool,
}

impl<'a> Reader<'a> {
    /// The bytes from `at` to the end of the file.
    fn from(&self, at: u64, what: &str) -> Result<&'a [u8], String> {
        usize::try_from(at)
            .ok()
            .and_then(|from| self.bytes.get(from..))
            .ok_or_else(|| format!("{what} (offset {at}) is outside the file"))
    }

    /// `len` bytes from `at`.
    fn slice(&self, at: u64, len: u64, what: &str) -> Result<&'a [u8], String> {
        self.from(at, what)?
            .get(..usize::try_from(len).unwrap_or(usize::MAX))
            .ok_or_else(|| format!("{what} ({len} bytes at offset {at}) is outside the file"))
    }

    /// An unsigned integer of `width` bytes (at most 8).
    fn uint(&self, at: u64, width: u64, what: &str) -> Result<u64, String> {
        let raw = self.slice(at, width, what)?;
        let fold = |value: u64, byte: &u8| value << 8 | u64::from(*byte);
        Ok(if self.little {
            raw.iter().rev().fold(0, fold)
        } else {
            raw.iter().fold(0, fold)
        })
    }

    /// The size of a word: 8 bytes in a 64-bit file, 4 in a 32-bit one.
    fn word_size(&self) -> u64 {
        if self.wide { 8 } else { 4 }
    }

    fn word(&self, at: u64, what: &str) -> Result<u64, String> {
        self.uint(at, self.word_size(), what)
    }

    /// The NUL-terminated string at `at`.
    fn string(&self, at: u64, what: &str) -> Result<String, String> {
        let rest = self.from(at, what)?;
        let end = rest
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| format!("{what} has no end inside the file"))?;
        Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
    }

    /// The program header at `at`.
    fn segment(&self, at: u64) -> Result<Segment, String> {
        let what = "a program header";
        let word = self.word_size();
        // 64-bit: type, flags, offset, vaddr, paddr, filesz. 32-bit: type, offset, vaddr, paddr,
        // filesz.
        let first = if self.wide { 8 } else { 4 };
        let field = |n: u64| self.word(at.saturating_add(first + n * word), what);
        Ok(Segment {
            kind: self.uint(at, 4, what)?,
            offset: field(0)?,
            vaddr: field(1)?,
            filesz: field(3)?,
        })
    }
}

/// Reads the ELF file `bytes`.
pub(crate) fn parse(bytes: &[u8]) -> Result<Elf, String> {
    if !is_elf(bytes) {
        return Err("no ELF magic".to_owned());
    }
    let ident = bytes
        .get(..16)
        .ok_or_else(|| format!("{} bytes, too short for an ELF header", bytes.len()))?;
    let wide = match ident[4] {
        1 => false,
        2 => true,
        other => return Err(format!("ELF class {other}, not 1 (32-bit) or 2 (64-bit)")),
    };
    let little = match ident[5] {
        1 => true,
        2 => false,
        other => {
            return Err(format!(
                "ELF data {other}, not 1 (little-endian) or 2 (big-endian)"
            ));
        }
    };
    let reader = Reader {
        bytes,
        little,
        wide,
    };
    let machine = u16::try_from(reader.uint(18, 2, "e_machine")?).unwrap_or_default();
    // After e_ident (16), e_type (2), e_machine (2), e_version (4) and e_entry: e_phoff, e_shoff,
    // e_flags (4), e_ehsize (2), e_phentsize (2) and e_phnum (2).
    let word = reader.word_size();
    let phoff = reader.word(24 + word, "e_phoff")?;
    let after = 24 + 3 * word + 4 + 2;
    let phentsize = reader.uint(after, 2, "e_phentsize")?;
    let phnum = reader.uint(after + 2, 2, "e_phnum")?;
    let minimum = if wide { 56 } else { 32 };
    if phnum > 0 && phentsize < minimum {
        return Err(format!(
            "program headers of {phentsize} bytes, fewer than the {minimum} of this class"
        ));
    }
    let mut segments = Vec::new();
    for n in 0..phnum {
        segments.push(reader.segment((n * phentsize).saturating_add(phoff))?);
    }
    let of_kind = |kind| segments.iter().find(|s| s.kind == kind);
    let interpreter = match of_kind(PT_INTERP) {
        Some(s) => {
            let raw = reader.slice(s.offset, s.filesz, "PT_INTERP")?;
            let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
            Some(String::from_utf8_lossy(&raw[..end]).into_owned())
        }
        None => None,
    };
    let needed = match of_kind(PT_DYNAMIC) {
        Some(dynamic) => needed(&reader, dynamic, &segments)?,
        None => Vec::new(),
    };
    Ok(Elf {
        class: ident[4],
        little_endian: little,
        machine,
        interpreter,
        needed,
    })
}

/// The `DT_NEEDED` names of the `PT_DYNAMIC` segment `dynamic`.
fn needed(
    reader: &Reader<'_>,
    dynamic: &Segment,
    segments: &[Segment],
) -> Result<Vec<String>, String> {
    let table = reader.slice(dynamic.offset, dynamic.filesz, "PT_DYNAMIC")?;
    let entry = 2 * reader.word_size();
    let (mut names, mut strtab) = (Vec::new(), None);
    for n in 0..u64::try_from(table.len()).unwrap_or_default() / entry {
        let at = dynamic.offset + n * entry;
        let tag = reader.word(at, "a dynamic entry")?;
        let value = reader.word(at + reader.word_size(), "a dynamic entry")?;
        match tag {
            DT_NULL => break,
            DT_NEEDED => names.push(value),
            DT_STRTAB => strtab = Some(value),
            _ => {}
        }
    }
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let strtab = strtab.ok_or_else(|| "DT_NEEDED without DT_STRTAB".to_owned())?;
    let load = segments
        .iter()
        .find(|s| s.kind == PT_LOAD && strtab >= s.vaddr && strtab - s.vaddr < s.filesz)
        .ok_or_else(|| format!("DT_STRTAB {strtab:#x} is in no PT_LOAD segment"))?;
    let base = load.offset.saturating_add(strtab - load.vaddr);
    names
        .into_iter()
        .map(|name| reader.string(base.saturating_add(name), "a DT_NEEDED name"))
        .collect()
}

#[cfg(test)]
pub(crate) mod fixture;
#[cfg(test)]
mod tests;
