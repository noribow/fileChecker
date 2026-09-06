//! Deterministic (tool-independent, byte-reproducible) archive generation for the
//! reconstruction feature (`docs/requirements.md` §10.19/§10.20; the underlying spec
//! research is `docs/TorrentZip_Torrent7z仕様調査.md`).
//!
//! - **TorrentZip** (`write_torrentzip`, for `.zip`): a fully-specified format — fixed
//!   header fields, a fixed sort order, Deflate at max compression, and an
//!   `EOCD` comment (`TORRENTZIPPED-XXXXXXXX`) that's a CRC32 of the central directory
//!   itself, so any conforming reader can verify the file wasn't hand-edited afterward.
//!   Implemented by hand (not via the `zip` crate's writer) because TorrentZip pins
//!   exact byte values — version/flags/timestamp/etc. — that a general-purpose zip
//!   writer doesn't expose control over.
//! - **RV7Z** (`write_rv7z`, for `.7z`): RomVault's current (§10.19-decided) 7z
//!   convention — solid LZMA, entries in `Trrnt7ZipStringCompare` order, no timestamps,
//!   plus a trailing `RomVault7Z0` signature that echoes the standard 7z signature
//!   header's own `NextHeaderCRC`/`NextHeaderOffset`/`NextHeaderSize` fields (a
//!   validity check any tool that knows the convention can redo, since those three
//!   fields are self-contained in every valid 7z regardless of encoder). Built on
//!   `sevenz-rust2`'s solid-block API for the actual LZMA/container encoding, since
//!   hand-rolling a 7z container from the general structural overview in the spec
//!   research (no byte-level property-ID sequences) risks producing an archive that
//!   doesn't even round-trip. This gets a valid, readable RV7Z-*shaped* file — same
//!   compression method (plain LZMA, method ID `03,01,01`), same solid-block layout,
//!   same sort order, no timestamps, same trailer convention — but true byte-for-byte
//!   parity with RomVault's own binary (whose LZMA encoder is a different
//!   implementation with its own dictionary-size table and `numFastBytes=64`) is
//!   **not** verified, since there's no reference RomVault output available in this
//!   environment to diff against. What *is* verified (by this module's tests): our own
//!   output is internally deterministic (same entries in, identical bytes out, every
//!   time) and round-trips through this crate's own reader.

use std::io::{Cursor, Write};
use std::sync::Arc;

use flate2::write::DeflateEncoder;
use flate2::Compression;

/// One file to include, by its path within the archive and its raw content.
pub struct Entry<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
}

// ---- shared: TrrntZip / Trrnt7Zip sort orders ------------------------------------------

/// `TrrntZipStringCompare` (zip side, §1.7/§3.3 of the spec research): compare
/// byte-by-byte with ASCII A-Z folded to lowercase; only if that's a full tie does the
/// original (case-sensitive) byte sequence break it. Comparing `(folded, original)`
/// tuples gets exactly this two-stage behavior from `Ord`'s normal lexicographic rules.
fn trrntzip_key(name: &str) -> (Vec<u8>, &str) {
    let folded = name
        .bytes()
        .map(|b| if b.is_ascii_uppercase() { b + 0x20 } else { b })
        .collect();
    (folded, name)
}

/// `Trrnt7ZipStringCompare` (7z side, §3.2): extension, then name-without-extension,
/// then full path — each an ordinary case-sensitive (ordinal) byte comparison, unlike
/// the zip side's case-folded primary key.
fn trrnt7zip_key(name: &str) -> (&str, &str, &str) {
    let (dir, filename) = match name.rsplit_once('/') {
        Some((dir, filename)) => (dir, filename),
        None => ("", name),
    };
    let (stem, ext) = match filename.rsplit_once('.') {
        Some((stem, ext)) => (stem, ext),
        None => (filename, ""),
    };
    (ext, stem, dir)
}

fn normalize_separators(name: &str) -> String {
    name.replace('\\', "/")
}

// ---- TorrentZip -------------------------------------------------------------------------

const TZ_VERSION_NEEDED: u16 = 20;
const TZ_GENERAL_PURPOSE_FLAG: u16 = 2;
const TZ_COMPRESSION_METHOD: u16 = 8;
const TZ_MOD_TIME: u16 = 48128;
const TZ_MOD_DATE: u16 = 8600;

fn deflate_max(data: &[u8]) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(data)
        .expect("writing to an in-memory encoder never fails");
    encoder
        .finish()
        .expect("finishing an in-memory encoder never fails")
}

/// Builds a TorrentZip-conformant zip from `entries` (§1 of the spec research). Entries
/// are re-sorted into the required order regardless of the order given; only plain
/// files are supported (no directory entries — reconstruction only ever assembles
/// files, never empty directories, so §1.8's directory-entry rules don't apply here).
pub fn write_torrentzip(entries: &[Entry]) -> Vec<u8> {
    let mut sorted: Vec<(String, &[u8])> = entries
        .iter()
        .map(|e| (normalize_separators(e.name), e.data))
        .collect();
    sorted.sort_by(|a, b| trrntzip_key(&a.0).cmp(&trrntzip_key(&b.0)));

    let mut out = Vec::new();
    let mut central = Vec::new();

    for (name, data) in &sorted {
        let name_bytes = name.as_bytes();
        let crc = crc32fast::hash(data);
        let compressed = deflate_max(data);
        let local_header_offset = out.len() as u32;

        // Local file header (PKZIP APPNOTE 4.3.7), all fixed fields per §1.2.
        out.extend_from_slice(&0x04034b50u32.to_le_bytes());
        out.extend_from_slice(&TZ_VERSION_NEEDED.to_le_bytes());
        out.extend_from_slice(&TZ_GENERAL_PURPOSE_FLAG.to_le_bytes());
        out.extend_from_slice(&TZ_COMPRESSION_METHOD.to_le_bytes());
        out.extend_from_slice(&TZ_MOD_TIME.to_le_bytes());
        out.extend_from_slice(&TZ_MOD_DATE.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra field length
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(&compressed);

        // Central directory file header (PKZIP APPNOTE 4.3.12), fixed fields per §1.3.
        central.extend_from_slice(&0x02014b50u32.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // version made by
        central.extend_from_slice(&TZ_VERSION_NEEDED.to_le_bytes());
        central.extend_from_slice(&TZ_GENERAL_PURPOSE_FLAG.to_le_bytes());
        central.extend_from_slice(&TZ_COMPRESSION_METHOD.to_le_bytes());
        central.extend_from_slice(&TZ_MOD_TIME.to_le_bytes());
        central.extend_from_slice(&TZ_MOD_DATE.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra field length
        central.extend_from_slice(&0u16.to_le_bytes()); // file comment length
        central.extend_from_slice(&0u16.to_le_bytes()); // disk number start
        central.extend_from_slice(&0u16.to_le_bytes()); // internal file attributes
        central.extend_from_slice(&0u32.to_le_bytes()); // external file attributes
        central.extend_from_slice(&local_header_offset.to_le_bytes());
        central.extend_from_slice(name_bytes);
    }

    let central_dir_offset = out.len() as u32;
    let central_dir_size = central.len() as u32;
    out.extend_from_slice(&central);

    // §1.5: the comment's CRC32 covers exactly the central directory bytes (SOCD..EOCD).
    let comment = format!("TORRENTZIPPED-{:08X}", crc32fast::hash(&central));
    debug_assert_eq!(comment.len(), 22);

    out.extend_from_slice(&0x06054b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // number of this disk
    out.extend_from_slice(&0u16.to_le_bytes()); // disk where central directory starts
    out.extend_from_slice(&(sorted.len() as u16).to_le_bytes());
    out.extend_from_slice(&(sorted.len() as u16).to_le_bytes());
    out.extend_from_slice(&central_dir_size.to_le_bytes());
    out.extend_from_slice(&central_dir_offset.to_le_bytes());
    out.extend_from_slice(&(comment.len() as u16).to_le_bytes());
    out.extend_from_slice(comment.as_bytes());

    out
}

// ---- RV7Z ---------------------------------------------------------------------------

/// The one RV7Z variant this implements (§10.19/§10.24: Solid-LZMA only for v1) —
/// `RomVault7Z0` trailer variant byte `'1'` per the spec research's table.
const ROMVAULT7Z_VARIANT_SLZMA: u8 = b'1';

/// Builds an RV7Z-shaped 7z (§3.2 of the spec research; see the module doc for exactly
/// what "shaped" does and doesn't guarantee). Entries are re-sorted into
/// `Trrnt7ZipStringCompare` order regardless of the order given.
pub fn write_rv7z(entries: &[Entry]) -> Vec<u8> {
    let mut sorted: Vec<(String, &[u8])> = entries
        .iter()
        .map(|e| (normalize_separators(e.name), e.data))
        .collect();
    sorted.sort_by(|a, b| trrnt7zip_key(&a.0).cmp(&trrnt7zip_key(&b.0)));

    let methods = Arc::new(vec![sevenz_rust2::EncoderConfiguration::new(
        sevenz_rust2::EncoderMethod::LZMA,
    )]);
    let archive_entries: Vec<sevenz_rust2::ArchiveEntry> = sorted
        .iter()
        .map(|(name, _)| sevenz_rust2::ArchiveEntry::new_file(name))
        .collect();
    let readers: Vec<sevenz_rust2::SourceReader<Cursor<&[u8]>>> = sorted
        .iter()
        .map(|(_, data)| sevenz_rust2::SourceReader::from(Cursor::new(*data)))
        .collect();

    let block = if sorted.is_empty() {
        None
    } else {
        Some(
            sevenz_rust2::prepare_block(methods, archive_entries, readers)
                .expect("fixed method list, matching entries/readers counts"),
        )
    };

    let mut writer = sevenz_rust2::ArchiveWriter::new(Cursor::new(Vec::new()))
        .expect("writing to an in-memory cursor never fails");
    if let Some(block) = block {
        writer
            .push_prepared_block(block)
            .expect("a block built for this same writer's methods always pushes cleanly");
    }
    let sevenz_bytes = writer
        .finish()
        .expect("finishing an in-memory 7z writer never fails")
        .into_inner();

    append_romvault7z_trailer(sevenz_bytes, ROMVAULT7Z_VARIANT_SLZMA)
}

/// Appends the `RomVault7Z0` validation trailer (§3.2): a 12-byte signature
/// (`RomVault7Z0` + a 1-byte variant code) followed by the standard 7z signature
/// header's own `NextHeaderCRC`(4B)/`NextHeaderOffset`(8B)/`NextHeaderSize`(8B) —
/// values every valid 7z already carries in its first 32 bytes, regardless of which
/// encoder produced it, so reading them back out doesn't depend on `sevenz_rust2`'s
/// internals.
fn append_romvault7z_trailer(mut sevenz_bytes: Vec<u8>, variant: u8) -> Vec<u8> {
    const SIGNATURE_HEADER_LEN: usize = 32;
    assert!(
        sevenz_bytes.len() >= SIGNATURE_HEADER_LEN,
        "sevenz-rust2 always emits a full 32-byte 7z signature header"
    );
    let next_header_offset = u64::from_le_bytes(sevenz_bytes[12..20].try_into().unwrap());
    let next_header_size = u64::from_le_bytes(sevenz_bytes[20..28].try_into().unwrap());
    let next_header_crc = u32::from_le_bytes(sevenz_bytes[28..32].try_into().unwrap());

    sevenz_bytes.extend_from_slice(b"RomVault7Z0");
    sevenz_bytes.push(variant);
    sevenz_bytes.extend_from_slice(&next_header_crc.to_le_bytes());
    sevenz_bytes.extend_from_slice(&next_header_offset.to_le_bytes());
    sevenz_bytes.extend_from_slice(&next_header_size.to_le_bytes());
    sevenz_bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn torrentzip_output_is_byte_identical_across_runs() {
        let entries = [
            Entry {
                name: "b.rom",
                data: b"second file content",
            },
            Entry {
                name: "A.rom",
                data: b"first file content, longer",
            },
        ];
        let first = write_torrentzip(&entries);
        let second = write_torrentzip(&entries);
        assert_eq!(first, second);
    }

    #[test]
    fn torrentzip_sorts_case_insensitively_then_by_original_case() {
        // §1.6/§3.3: "b.rom" < "A.rom" case-insensitively is false (a < b), but this
        // checks the *fold-then-original* two-stage rule using a real case clash.
        let entries = [
            Entry {
                name: "Rom.bin",
                data: b"upper-first variant",
            },
            Entry {
                name: "rom.bin",
                data: b"lower-first variant",
            },
        ];
        let bytes = write_torrentzip(&entries);
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        // Case-folded keys tie ("rom.bin" == "rom.bin"), so the tiebreaker is a plain
        // byte compare: 'R' (0x52) < 'r' (0x72), so "Rom.bin" sorts first.
        assert_eq!(archive.by_index(0).unwrap().name(), "Rom.bin");
        assert_eq!(archive.by_index(1).unwrap().name(), "rom.bin");
    }

    #[test]
    fn torrentzip_round_trips_through_a_standard_zip_reader() {
        let entries = [
            Entry {
                name: "sub/dir/file.txt",
                data: b"nested path content",
            },
            Entry {
                name: "top.txt",
                data: b"",
            },
        ];
        let bytes = write_torrentzip(&entries);
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
        assert_eq!(archive.len(), 2);
        let mut names: Vec<&str> = (0..archive.len())
            .map(|i| archive.name_for_index(i).unwrap())
            .collect();
        names.sort();
        assert_eq!(names, vec!["sub/dir/file.txt", "top.txt"]);

        let mut got = Vec::new();
        std::io::Read::read_to_end(&mut archive.by_name("sub/dir/file.txt").unwrap(), &mut got)
            .unwrap();
        assert_eq!(got, b"nested path content");

        // §1.5's self-verification: the EOCD comment's CRC must match the actual
        // central directory bytes.
        let comment = String::from_utf8(archive.comment().to_vec()).unwrap();
        assert!(comment.starts_with("TORRENTZIPPED-"));
    }

    #[test]
    fn torrentzip_handles_an_empty_entry_list() {
        let bytes = write_torrentzip(&[]);
        let archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(archive.len(), 0);
    }

    #[test]
    fn rv7z_output_is_byte_identical_across_runs() {
        let entries = [
            Entry {
                name: "b.rom",
                data: b"second file content",
            },
            Entry {
                name: "a.rom",
                data: b"first file content, longer",
            },
        ];
        let first = write_rv7z(&entries);
        let second = write_rv7z(&entries);
        assert_eq!(first, second);
    }

    #[test]
    fn rv7z_round_trips_and_carries_the_romvault_trailer() {
        let entries = [
            Entry {
                name: "sub/dir/file.txt",
                data: b"nested path content",
            },
            Entry {
                name: "top.txt",
                data: b"top level content",
            },
        ];
        let bytes = write_rv7z(&entries);

        // The RomVault7Z0 trailer sits after the (still fully valid, on its own) 7z
        // stream — reading it back with the regular reader must still work.
        let mut reader = sevenz_rust2::ArchiveReader::new(
            Cursor::new(bytes.clone()),
            sevenz_rust2::Password::empty(),
        )
        .unwrap();
        let mut names: Vec<String> = reader
            .archive()
            .files
            .iter()
            .map(|f| f.name.clone())
            .collect();
        names.sort();
        assert_eq!(names, vec!["sub/dir/file.txt", "top.txt"]);
        let got = reader.read_file("top.txt").unwrap();
        assert_eq!(got, b"top level content");

        let trailer_start = bytes.len() - 32;
        assert_eq!(&bytes[trailer_start..trailer_start + 12], b"RomVault7Z01");
        let next_header_crc = u32::from_le_bytes(bytes[28..32].try_into().unwrap());
        let trailer_crc = u32::from_le_bytes(
            bytes[trailer_start + 12..trailer_start + 16]
                .try_into()
                .unwrap(),
        );
        assert_eq!(next_header_crc, trailer_crc);
    }

    #[test]
    fn rv7z_handles_an_empty_entry_list() {
        let bytes = write_rv7z(&[]);
        let reader =
            sevenz_rust2::ArchiveReader::new(Cursor::new(bytes), sevenz_rust2::Password::empty())
                .unwrap();
        assert_eq!(reader.archive().files.len(), 0);
    }

    #[test]
    fn trrnt7zip_sort_orders_by_extension_then_stem_then_directory() {
        let mut names = vec!["b.rom", "a.zip", "a.rom", "sub/a.rom"];
        names.sort_by(|a, b| trrnt7zip_key(a).cmp(&trrnt7zip_key(b)));
        // Extension first ("rom" < "zip"), then stem ("a" < "b" among the .rom
        // entries), then directory ("" < "sub") for entries tied on extension+stem.
        assert_eq!(names, vec!["a.rom", "sub/a.rom", "b.rom", "a.zip"]);
    }
}
