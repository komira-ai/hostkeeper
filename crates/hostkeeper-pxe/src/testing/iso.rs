//! ISO 9660 images with Rock Ridge names, made in memory: the volume descriptors at sector 16, the
//! directories, then the files, each directory's records packed so none crosses a sector. A
//! record's ISO 9660 name is a short counter (the reader never reads it); its Rock Ridge name is
//! in `NM` entries. [`Image`] holds the knobs the reader's tests turn.

use std::collections::BTreeMap;

const SECTOR: usize = 2048;

/// What goes into an image.
#[derive(Clone, Debug, Default)]
pub(crate) struct Image {
    /// Each file by its path (parts separated by `/`) and its bytes; the directories on the way
    /// are made.
    pub files: Vec<(String, Vec<u8>)>,
    /// Leave out the root's `SP` entry: an image with no Rock Ridge names.
    pub no_rock_ridge: bool,
    /// Write each name as two `NM` entries, the first with the CONTINUE flag.
    pub split_names: bool,
    /// Put each file's `NM` entries in a continuation area (a `CE` entry), with a `PX` entry
    /// before and an `ST` entry after the `CE`.
    pub continued_names: bool,
    /// The paths of the files recorded with the multi-extent flag.
    pub multi_extent: Vec<String>,
    /// Write a boot record descriptor (type 0) before the primary one.
    pub boot_record_first: bool,
}

/// A both-endian 32-bit field.
fn both32(value: u32) -> [u8; 8] {
    let mut out = [0_u8; 8];
    out[..4].copy_from_slice(&value.to_le_bytes());
    out[4..].copy_from_slice(&value.to_be_bytes());
    out
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).expect("fits the image")
}

/// A directory record.
pub(crate) fn record(
    block: u32,
    bytes: u32,
    flags: u8,
    iso_name: &[u8],
    system_use: &[u8],
) -> Vec<u8> {
    let pad = usize::from(iso_name.len().is_multiple_of(2));
    let length = 33 + iso_name.len() + pad + system_use.len();
    let mut out = vec![u8::try_from(length).expect("a record under 256 bytes"), 0];
    out.extend(both32(block));
    out.extend(both32(bytes));
    out.extend([0_u8; 7]);
    out.extend([flags, 0, 0]);
    out.extend([1, 0, 0, 1]);
    out.push(u8::try_from(iso_name.len()).expect("a short name"));
    out.extend_from_slice(iso_name);
    out.extend(std::iter::repeat_n(0, pad));
    out.extend_from_slice(system_use);
    out
}

/// A system use entry.
pub(crate) fn entry(signature: &[u8; 2], data: &[u8]) -> Vec<u8> {
    let mut out = signature.to_vec();
    out.push(u8::try_from(4 + data.len()).expect("a short entry"));
    out.push(1);
    out.extend_from_slice(data);
    out
}

/// The `NM` entries of `name`: one, or, split, two halves, the first with the CONTINUE flag.
fn name_entries(name: &str, split: bool) -> Vec<u8> {
    let bytes = name.as_bytes();
    let nm = |flags: u8, part: &[u8]| entry(b"NM", &[[flags].as_slice(), part].concat());
    match split {
        true => {
            let (first, rest) = bytes.split_at(bytes.len() / 2);
            [nm(1, first), nm(0, rest)].concat()
        }
        false => nm(0, bytes),
    }
}

/// A `CE` entry pointing at `length` bytes at `offset` of `block`.
pub(crate) fn continuation(block: u32, offset: u32, length: u32) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend(both32(block));
    data.extend(both32(offset));
    data.extend(both32(length));
    entry(b"CE", &data)
}

/// The sectors a directory of these records takes, records never crossing a sector.
fn layout(records: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for record in records {
        let used = out.len() % SECTOR;
        if used + record.len() > SECTOR {
            out.resize(out.len() + SECTOR - used, 0);
        }
        out.extend_from_slice(record);
    }
    let size = out.len().div_ceil(SECTOR).max(1) * SECTOR;
    out.resize(size, 0);
    out
}

/// A directory as it is laid out: its sector, its size, and its children by name.
struct Placed {
    block: u32,
    bytes: u32,
}

impl Image {
    /// An image of `files`, with every knob off.
    pub(crate) fn of(files: &[(&str, &[u8])]) -> Image {
        Image {
            files: files
                .iter()
                .map(|(path, bytes)| ((*path).to_owned(), bytes.to_vec()))
                .collect(),
            ..Image::default()
        }
    }

    /// The bytes of the image.
    pub(crate) fn build(&self) -> Vec<u8> {
        // Each directory by its path, the root's `""`, and its children by name, each with
        // whether it is a directory.
        let mut tree: BTreeMap<String, BTreeMap<String, bool>> = BTreeMap::new();
        for (path, _) in &self.files {
            let parts: Vec<&str> = path.split('/').collect();
            for (at, part) in parts.iter().enumerate() {
                tree.entry(parts[..at].join("/"))
                    .or_default()
                    .insert((*part).to_owned(), at + 1 < parts.len());
            }
        }
        // Two passes: the first sizes every directory (sizes depend on names only), the second
        // writes the records with the sectors the first pass gave.
        let dirs: Vec<(String, Vec<(String, bool)>)> = tree
            .into_iter()
            .map(|(path, children)| (path, children.into_iter().collect()))
            .collect();
        let first_dir = 16 + 1 + usize::from(self.boot_record_first) + 1;
        let mut placed: BTreeMap<String, Placed> = BTreeMap::new();
        let mut next = first_dir;
        for (path, children) in &dirs {
            let records = self.records(
                path,
                children,
                &BTreeMap::new(),
                &BTreeMap::new(),
                0,
                &mut 0,
            );
            let bytes = layout(&records).len();
            placed.insert(
                path.clone(),
                Placed {
                    block: to_u32(next),
                    bytes: to_u32(bytes),
                },
            );
            next += bytes / SECTOR;
        }
        let continuation_block = next;
        next += 1;
        let mut file_blocks = BTreeMap::new();
        for (path, bytes) in &self.files {
            file_blocks.insert(path.clone(), (to_u32(next), to_u32(bytes.len())));
            next += bytes.len().div_ceil(SECTOR).max(1);
        }
        let mut image = vec![0_u8; next * SECTOR];
        let mut descriptor_at = 16;
        if self.boot_record_first {
            image[16 * SECTOR] = 0;
            image[16 * SECTOR + 1..16 * SECTOR + 6].copy_from_slice(b"CD001");
            descriptor_at = 17;
        }
        let pvd = descriptor_at * SECTOR;
        image[pvd] = 1;
        image[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        image[pvd + 6] = 1;
        image[pvd + 80..pvd + 88].copy_from_slice(&both32(to_u32(next)));
        image[pvd + 128..pvd + 130].copy_from_slice(&2048_u16.to_le_bytes());
        image[pvd + 130..pvd + 132].copy_from_slice(&2048_u16.to_be_bytes());
        let root_placed = &placed[""];
        let root_record = record(root_placed.block, root_placed.bytes, 2, &[0], &[]);
        image[pvd + 156..pvd + 156 + root_record.len()].copy_from_slice(&root_record);
        let terminator = (descriptor_at + 1) * SECTOR;
        image[terminator] = 255;
        image[terminator + 1..terminator + 6].copy_from_slice(b"CD001");
        let mut continued = Vec::new();
        let mut offset = 0_u32;
        for (path, children) in &dirs {
            let records = self.records(
                path,
                children,
                &placed_blocks(&placed),
                &file_blocks,
                to_u32(continuation_block),
                &mut offset,
            );
            let laid = layout(&records);
            let at = placed[path].block as usize * SECTOR;
            image[at..at + laid.len()].copy_from_slice(&laid);
            if self.continued_names {
                for (name, _) in children {
                    continued.push(name_entries(name, self.split_names));
                }
            }
        }
        assert!(
            offset as usize <= SECTOR,
            "the continuation area is one sector"
        );
        if self.continued_names {
            let mut offset = 0;
            for names in continued {
                let at = continuation_block * SECTOR + offset;
                image[at..at + names.len()].copy_from_slice(&names);
                offset += names.len();
            }
        }
        for (path, bytes) in &self.files {
            let (block, _) = file_blocks[path];
            let at = block as usize * SECTOR;
            image[at..at + bytes.len()].copy_from_slice(bytes);
        }
        image
    }

    /// The records of the directory `path`: `.`, `..`, then each child.
    fn records(
        &self,
        path: &str,
        children: &[(String, bool)],
        dirs: &BTreeMap<String, (u32, u32)>,
        files: &BTreeMap<String, (u32, u32)>,
        continuation_block: u32,
        offset: &mut u32,
    ) -> Vec<Vec<u8>> {
        let (block, bytes) = dirs.get(path).copied().unwrap_or((0, 0));
        let sp = match path.is_empty() & !self.no_rock_ridge {
            true => entry(b"SP", &[0xBE, 0xEF, 0]),
            false => Vec::new(),
        };
        let mut records = vec![
            record(block, bytes, 2, &[0], &sp),
            record(block, bytes, 2, &[1], &[]),
        ];
        for (index, (name, is_dir)) in children.iter().enumerate() {
            let full = match path.is_empty() {
                true => name.clone(),
                false => format!("{path}/{name}"),
            };
            let (block, bytes, mut flags) = match is_dir {
                true => {
                    let (block, bytes) = dirs.get(&full).copied().unwrap_or((0, 0));
                    (block, bytes, 2)
                }
                false => {
                    let (block, bytes) = files.get(&full).copied().unwrap_or((0, 0));
                    (block, bytes, 0)
                }
            };
            if self.multi_extent.contains(&full) {
                flags |= 0x80;
            }
            let names = name_entries(name, self.split_names);
            let system_use = match self.continued_names {
                true => {
                    let mut su = entry(b"PX", &[0; 32]);
                    su.extend(continuation(
                        continuation_block,
                        *offset,
                        to_u32(names.len()),
                    ));
                    su.extend(entry(b"ST", &[]));
                    *offset += to_u32(names.len());
                    su
                }
                false => names,
            };
            let iso_name = format!("N{index}");
            records.push(record(
                block,
                bytes,
                flags,
                iso_name.as_bytes(),
                &system_use,
            ));
        }
        records
    }
}

fn placed_blocks(placed: &BTreeMap<String, Placed>) -> BTreeMap<String, (u32, u32)> {
    placed
        .iter()
        .map(|(path, at)| (path.clone(), (at.block, at.bytes)))
        .collect()
}

/// The offset of the primary volume descriptor of an image [`Image::build`] made without a boot
/// record.
pub(crate) const PVD: usize = 16 * SECTOR;
