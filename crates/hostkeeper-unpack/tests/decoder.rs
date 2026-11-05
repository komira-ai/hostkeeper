//! The zstd frame: real compressed blocks, the content checksum, the declared content size,
//! single-segment windows, and the frame header fields the decoder refuses or must not ignore.

mod support;

use hostkeeper_hook::{Class, Outcome};
use hostkeeper_unpack::{MAX_WINDOW_BYTES, Platform, Rule, UnpackError, unpack};
use ruzstd::encoding::{CompressionLevel, compress_to_vec};
use support::{Rig, VERSION, WINDOW_1M, block_header, limits, valid_tar};

/// The valid package, compressed by a real encoder (compressed blocks, a content checksum).
fn compressed() -> Vec<u8> {
    let tar = valid_tar().end().bytes();
    compress_to_vec(&tar[..], CompressionLevel::Fastest)
}

/// A frame of `header` (the descriptor and the fields after the magic) holding `data` in raw
/// blocks and then `zeros` zero bytes in RLE blocks.
fn frame(header: &[u8], data: &[u8], zeros: u64) -> Vec<u8> {
    let mut out = vec![0x28, 0xb5, 0x2f, 0xfd];
    out.extend_from_slice(header);
    for chunk in data.chunks(128 * 1024) {
        out.extend_from_slice(&block_header(false, 0, chunk.len() as u32));
        out.extend_from_slice(chunk);
    }
    let mut left = zeros;
    while left > 0 {
        let n = left.min(128 * 1024);
        out.extend_from_slice(&block_header(false, 1, n as u32));
        out.push(0);
        left -= n;
    }
    out.extend_from_slice(&block_header(true, 0, 0));
    out
}

/// A single-segment frame header that declares `size` content bytes (and so a window of
/// `size`), in an eight-byte content-size field.
fn single_segment(size: u64) -> Vec<u8> {
    let mut header = vec![0xe0];
    header.extend_from_slice(&size.to_le_bytes());
    header
}

fn run(name: &str, archive: &[u8]) -> (Rig, Result<hostkeeper_unpack::Unpacked, UnpackError>) {
    let rig = Rig::new(name);
    let (path, order) = rig.archive("a.tar.zst", archive);
    let result = unpack(&path, &order, &limits(), &rig.host(), Platform::Linux);
    (rig, result)
}

/// Catches a decoder that handles only stored and repeated blocks: a package compressed by a
/// real encoder, with compressed blocks and a content checksum, unpacks to the same tree as the
/// stored one.
#[test]
fn a_package_of_compressed_blocks_with_a_checksum_unpacks() {
    let archive = compressed();
    let tar = valid_tar().end().bytes();
    assert!(
        archive.len() * 4 < tar.len(),
        "{} of {}",
        archive.len(),
        tar.len()
    );
    let descriptor = archive[4];
    assert_eq!(
        descriptor & 0x04,
        0x04,
        "the frame carries a content checksum"
    );
    let (rig, unpacked) = run("d-compressed", &archive);
    let unpacked = unpacked.unwrap();
    let (_, stored) = run("d-stored", &support::zstd(&tar));
    assert_eq!(
        unpacked.seal.tree_digest(),
        stored.unwrap().seal.tree_digest()
    );
    assert!(rig.service().join(VERSION).is_dir());
}

/// Catches a frame that is accepted though its bytes are not what it says: a content checksum
/// that does not match, a compressed block of garbage, a declared content size the frame does
/// not decompress to, a dictionary, a skippable frame and a reserved header bit are each
/// refused as `bad_archive` by the decoder rule, and leave nothing.
#[test]
fn a_frame_that_is_not_what_it_declares_is_refused() {
    let tar = valid_tar().end().bytes();
    let mut bad_sum = compressed();
    let end = bad_sum.len();
    bad_sum[end - 1] ^= 0xff;
    let mut garbage = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, WINDOW_1M];
    garbage.extend_from_slice(&block_header(true, 2, 16));
    garbage.extend_from_slice(&[0xff; 16]);
    let mut dictionary = vec![0x28, 0xb5, 0x2f, 0xfd, 0x01, WINDOW_1M, 7];
    dictionary.extend_from_slice(&block_header(true, 0, 0));
    let mut skippable = vec![0x50, 0x2a, 0x4d, 0x18, 4, 0, 0, 0, 1, 2, 3, 4];
    skippable.extend_from_slice(&support::zstd(&tar));
    let rows: Vec<(&str, Vec<u8>)> = vec![
        ("a content checksum that does not match", bad_sum),
        ("a compressed block of garbage", garbage),
        (
            "a declared content size larger than the content",
            frame(&single_segment(tar.len() as u64 + 512), &tar, 0),
        ),
        (
            "a declared content size smaller than the content",
            frame(&[0x40, WINDOW_1M, 0, 0], &tar, 0),
        ),
        ("a dictionary", dictionary),
        ("a skippable frame", skippable),
        ("a reserved header bit", frame(&[0x08, WINDOW_1M], &tar, 0)),
    ];
    for (name, archive) in rows {
        let (rig, result) = run("d-refused", &archive);
        let err = result.unwrap_err();
        assert!(
            matches!(
                &err,
                UnpackError::BadArchive {
                    rule: Rule::Decoder(_),
                    ..
                }
            ),
            "{name}: {err:?}"
        );
        assert_eq!(
            err.outcome(),
            (Outcome::Failure, Class::Release, "bad_archive"),
            "{name}"
        );
        assert!(rig.listing().is_empty(), "{name}: {:?}", rig.listing());
    }
}

/// Catches a single-segment frame escaping the window cap, whose window is its content size:
/// one that declares exactly the cap and holds that many bytes unpacks, one byte more is
/// refused by the window rule before a block is read; and a two-byte content-size field that
/// agrees with the content unpacks.
#[test]
fn a_single_segment_window_is_its_content_size_under_the_cap() {
    let tar = valid_tar().end().bytes();
    let at_cap = frame(
        &single_segment(MAX_WINDOW_BYTES),
        &tar,
        MAX_WINDOW_BYTES - tar.len() as u64,
    );
    let (_, result) = run("d-single-cap", &at_cap);
    result.unwrap();
    let over = frame(&single_segment(MAX_WINDOW_BYTES + 1), &tar, 0);
    let (rig, result) = run("d-single-over", &over);
    let err = result.unwrap_err();
    assert!(
        matches!(&err, UnpackError::BadArchive { rule: Rule::Window { requested }, at: 0, .. }
            if *requested == MAX_WINDOW_BYTES + 1),
        "{err:?}"
    );
    assert!(rig.listing().is_empty());
    let size = (tar.len() as u64 - 256) as u16;
    let mut header = vec![0x40, WINDOW_1M];
    header.extend_from_slice(&size.to_le_bytes());
    let (_, result) = run("d-two-byte-size", &frame(&header, &tar, 0));
    result.unwrap();
}
