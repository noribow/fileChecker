//! External reference-set import (`docs/requirements.md` §10.18): MAME's two published
//! XML/DTD formats (`softwarelist.dtd`, `mame.dtd`), the only external tools surveyed
//! so far (see `docs/外部形式マッピング案.md`) — adapters for other tools are a future
//! version's work. Each format gets its own field-mapping/exclusion logic per §10.18
//! point 8 ("don't reuse a mapping table across formats just because an element name
//! matches"); only the shared exclusion *rules* the spec itself states apply to both
//! formats are factored out (`is_excluded`), not element-extraction code.
//!
//! Only `crc32`/`sha1` are ever populated on the imported `reference_file` rows —
//! neither DTD carries an md5 or sha256 equivalent, so those stay `NULL` (§10.12's
//! NULL-tolerant design already covers "match whichever algorithms are present").

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::db::{repo, Connection};

#[derive(Debug)]
pub enum ImportError {
    Io(std::io::Error),
    Xml(String),
    Db(rusqlite::Error),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::Io(e) => write!(f, "入力ファイルを読み込めません: {e}"),
            ImportError::Xml(msg) => write!(f, "XML解析エラー: {msg}"),
            ImportError::Db(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<std::io::Error> for ImportError {
    fn from(e: std::io::Error) -> Self {
        ImportError::Io(e)
    }
}

impl From<rusqlite::Error> for ImportError {
    fn from(e: rusqlite::Error) -> Self {
        ImportError::Db(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MameFormat {
    SoftwareList,
    MachineList,
}

impl MameFormat {
    /// The `reference_set.source_format` value — also the format-mapping-table
    /// identity §10.18 point 8 requires stay distinct per format.
    pub fn format_id(self) -> &'static str {
        match self {
            MameFormat::SoftwareList => "mame-softwarelist",
            MameFormat::MachineList => "mame-machinelist",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "mame-softwarelist" => Some(MameFormat::SoftwareList),
            "mame-machinelist" => Some(MameFormat::MachineList),
            _ => None,
        }
    }
}

/// §10.18 point 3: the user must explicitly say which physical archive layout the
/// target folder/media uses — it can't be auto-detected from the XML alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMode {
    Merged,
    Split,
}

pub struct ImportOptions {
    /// §10.18 point 2: `status=baddump` is excluded unless explicitly opted in.
    pub include_baddump: bool,
    /// Only meaningful for `MameFormat::MachineList` (softwarelist has no
    /// merge/romof concept at all).
    pub merge_mode: MergeMode,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ImportSummary {
    pub reference_set_id: i64,
    pub imported_count: usize,
    pub excluded_count: usize,
}

/// One `rom`/`disk` element as read off the XML, before §10.18's exclusion rules and
/// path resolution are applied. Fields that don't exist in a given format's DTD are
/// simply left at their "never excludes/never merges" default by that format's parser.
struct RawEntry {
    container: String,
    name: String,
    size: Option<i64>,
    crc32_hex: Option<String>,
    sha1_hex: Option<String>,
    status: Option<String>,
    loadflag: Option<String>,
    bios: bool,
    optional: bool,
    writeable: bool,
    merge: Option<String>,
    machine_isdevice: bool,
    is_disk: bool,
}

/// Parses `xml_path` and writes a new `reference_set` (+ `reference_file` rows) from
/// the entries that survive §10.18's exclusion rules.
pub fn import_mame_reference_set(
    conn: &mut Connection,
    format: MameFormat,
    xml_path: &Path,
    name: &str,
    options: &ImportOptions,
    created_at: i64,
) -> Result<ImportSummary, ImportError> {
    let xml = std::fs::read_to_string(xml_path)?;
    let (raw_entries, romof_map) = match format {
        MameFormat::SoftwareList => (parse_softwarelist(&xml)?, HashMap::new()),
        MameFormat::MachineList => parse_machinelist(&xml)?,
    };

    let reference_set_id = repo::insert_reference_set(
        conn,
        name,
        format.format_id(),
        Some(&xml_path.to_string_lossy()),
        None,
        None,
        created_at,
    )?;

    let mut imported_count = 0usize;
    let mut excluded_count = 0usize;
    // Merged mode (§10.18 point 3) can legitimately resolve two different raw entries
    // to the same path: a machine's own literal rom and a clone's merge="..." pointer
    // to that exact same file both end up at the parent's archive path. The second one
    // is redundant, not new information — `reference_file` also enforces this via its
    // UNIQUE(reference_set_id, path), so silently dropping the repeat (keeping
    // whichever copy is seen first, i.e. the owning machine's own entry when it
    // appears before its clones, as MAME datfiles conventionally order them) is both
    // necessary and correct rather than a real "different file happens to collide".
    let mut seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let tx = conn.transaction()?;
        for entry in &raw_entries {
            if is_excluded(entry, options) {
                excluded_count += 1;
                continue;
            }
            let path = resolve_path(entry, &romof_map, options);
            if !seen_paths.insert(path.clone()) {
                excluded_count += 1;
                continue;
            }
            let crc32 = entry
                .crc32_hex
                .as_deref()
                .and_then(|h| u32::from_str_radix(h, 16).ok());
            let sha1 = entry.sha1_hex.as_deref().and_then(decode_hex);
            repo::insert_reference_file(
                &tx,
                &repo::NewReferenceFile {
                    reference_set_id,
                    path: &path,
                    size: entry.size.unwrap_or(0),
                    crc32,
                    md5: None,
                    sha1: sha1.as_deref(),
                    sha256: None,
                },
            )?;
            imported_count += 1;
        }
        tx.commit()?;
    }

    Ok(ImportSummary {
        reference_set_id,
        imported_count,
        excluded_count,
    })
}

/// §10.18's exclusion rules, shared verbatim across both formats since the spec itself
/// states most of them apply identically to both ("事例1・2共通"). A rule that only
/// exists in one DTD (e.g. `bios`/`optional`) simply never fires for the other format,
/// since that format's parser never sets the corresponding field to a triggering value.
fn is_excluded(e: &RawEntry, options: &ImportOptions) -> bool {
    if let Some(loadflag) = &e.loadflag {
        if matches!(loadflag.as_str(), "fill" | "reload" | "continue" | "ignore") {
            return true;
        }
    }
    match e.status.as_deref() {
        Some("nodump") => return true,
        Some("baddump") if !options.include_baddump => return true,
        _ => {}
    }
    if e.machine_isdevice {
        return true;
    }
    if e.is_disk && e.writeable {
        return true;
    }
    if e.bios || e.optional {
        return true;
    }
    false
}

/// §10.18 point 3: split mode ignores `merge` entirely; merged mode follows `merge` +
/// the owning machine's `romof` to the parent machine's archive. Only ever takes the
/// merged branch for entries that actually carry a `merge` attribute (softwarelist
/// entries never do, so this is always a no-op passthrough for that format).
fn resolve_path(e: &RawEntry, romof: &HashMap<String, String>, options: &ImportOptions) -> String {
    if options.merge_mode == MergeMode::Merged {
        if let Some(merge_name) = &e.merge {
            if let Some(parent) = romof.get(&e.container) {
                return format!("{parent}.zip/{merge_name}");
            }
        }
    }
    format!("{}.zip/{}", e.container, e.name)
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn attr_value(e: &BytesStart, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| {
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

fn is_yes(v: Option<String>) -> bool {
    v.as_deref() == Some("yes")
}

fn xml_err(e: quick_xml::Error) -> ImportError {
    ImportError::Xml(e.to_string())
}

/// `hash/softwarelist.dtd`: `<softwarelist><software name=...><part><dataarea><rom .../>
/// </dataarea><diskarea><disk .../></diskarea></part></software></softwarelist>`. Path
/// is always `{software@name}.zip/{name}` — this format has no merge/romof concept.
fn parse_softwarelist(xml: &str) -> Result<Vec<RawEntry>, ImportError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut entries = Vec::new();
    let mut current_software: Option<String> = None;

    loop {
        match reader.read_event().map_err(xml_err)? {
            Event::Eof => break,
            Event::Start(e) => match e.name().as_ref() {
                "software" => current_software = attr_value(&e, "name"),
                "rom" => {
                    push_softwarelist_entry(&mut entries, &e, current_software.as_deref(), false)
                }
                "disk" => {
                    push_softwarelist_entry(&mut entries, &e, current_software.as_deref(), true)
                }
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                "rom" => {
                    push_softwarelist_entry(&mut entries, &e, current_software.as_deref(), false)
                }
                "disk" => {
                    push_softwarelist_entry(&mut entries, &e, current_software.as_deref(), true)
                }
                _ => {}
            },
            Event::End(e) if e.name().as_ref() == "software" => current_software = None,
            _ => {}
        }
    }
    Ok(entries)
}

fn push_softwarelist_entry(
    entries: &mut Vec<RawEntry>,
    e: &BytesStart,
    container: Option<&str>,
    is_disk: bool,
) {
    let (Some(container), Some(name)) = (container, attr_value(e, "name")) else {
        return;
    };
    entries.push(RawEntry {
        container: container.to_string(),
        name,
        size: attr_value(e, "size").and_then(|s| s.parse().ok()),
        crc32_hex: attr_value(e, "crc"),
        sha1_hex: attr_value(e, "sha1"),
        status: attr_value(e, "status"),
        loadflag: attr_value(e, "loadflag"),
        bios: false,
        optional: false,
        writeable: is_disk
            && is_yes(attr_value(e, "writeable").or_else(|| attr_value(e, "writable"))),
        merge: None,
        machine_isdevice: false,
        is_disk,
    });
}

/// `mame.dtd`: `<mame><machine name=... romof=... isdevice=...><rom .../><disk .../>
/// </machine></mame>`. Collects every machine's `romof` alongside the entries in one
/// pass — path resolution (which needs the full romof map) happens afterward.
fn parse_machinelist(xml: &str) -> Result<(Vec<RawEntry>, HashMap<String, String>), ImportError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut entries = Vec::new();
    let mut romof_map = HashMap::new();
    let mut current_machine: Option<String> = None;
    let mut current_isdevice = false;

    loop {
        match reader.read_event().map_err(xml_err)? {
            Event::Eof => break,
            Event::Start(e) => match e.name().as_ref() {
                "machine" => {
                    let machine_name = attr_value(&e, "name");
                    current_isdevice = is_yes(attr_value(&e, "isdevice"));
                    if let (Some(n), Some(romof)) = (&machine_name, attr_value(&e, "romof")) {
                        romof_map.insert(n.clone(), romof);
                    }
                    current_machine = machine_name;
                }
                "rom" => push_machine_entry(
                    &mut entries,
                    &e,
                    current_machine.as_deref(),
                    false,
                    current_isdevice,
                ),
                "disk" => push_machine_entry(
                    &mut entries,
                    &e,
                    current_machine.as_deref(),
                    true,
                    current_isdevice,
                ),
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                "machine" => {
                    if let (Some(n), Some(romof)) =
                        (attr_value(&e, "name"), attr_value(&e, "romof"))
                    {
                        romof_map.insert(n, romof);
                    }
                }
                "rom" => push_machine_entry(
                    &mut entries,
                    &e,
                    current_machine.as_deref(),
                    false,
                    current_isdevice,
                ),
                "disk" => push_machine_entry(
                    &mut entries,
                    &e,
                    current_machine.as_deref(),
                    true,
                    current_isdevice,
                ),
                _ => {}
            },
            Event::End(e) if e.name().as_ref() == "machine" => {
                current_machine = None;
                current_isdevice = false;
            }
            _ => {}
        }
    }
    Ok((entries, romof_map))
}

fn push_machine_entry(
    entries: &mut Vec<RawEntry>,
    e: &BytesStart,
    container: Option<&str>,
    is_disk: bool,
    machine_isdevice: bool,
) {
    let (Some(container), Some(name)) = (container, attr_value(e, "name")) else {
        return;
    };
    entries.push(RawEntry {
        container: container.to_string(),
        name,
        size: attr_value(e, "size").and_then(|s| s.parse().ok()),
        crc32_hex: attr_value(e, "crc"),
        sha1_hex: attr_value(e, "sha1"),
        status: attr_value(e, "status"),
        loadflag: attr_value(e, "loadflag"),
        bios: attr_value(e, "bios").is_some(),
        optional: is_yes(attr_value(e, "optional")),
        writeable: is_disk
            && is_yes(attr_value(e, "writeable").or_else(|| attr_value(e, "writable"))),
        merge: attr_value(e, "merge"),
        machine_isdevice,
        is_disk,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    fn now() -> i64 {
        1_700_000_000_000
    }

    fn write_xml(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    fn options(include_baddump: bool, merge_mode: MergeMode) -> ImportOptions {
        ImportOptions {
            include_baddump,
            merge_mode,
        }
    }

    /// Golden sample shaped after `docs/外部形式マッピング案.md`'s softwarelist.dtd
    /// walkthrough: one software with a normal rom, one excluded on every §10.18 rule
    /// that applies to this format, and a normal + a writeable (excluded) disk.
    const SOFTWARELIST_XML: &str = r#"<?xml version="1.0"?>
<softwarelist name="example" description="Example Software List">
  <software name="game1" cloneof="">
    <description>Game One</description>
    <part name="cart" interface="cart">
      <dataarea name="rom" size="65536">
        <rom name="game1.bin" size="65536" crc="12345678" sha1="da39a3ee5e6b4b0d3255bfef95601890afd80709" status="good"/>
        <rom name="reload_part.bin" size="1024" crc="aaaaaaaa" loadflag="reload"/>
        <rom name="fill_byte.bin" size="1" loadflag="fill"/>
        <rom name="broken.bin" size="512" status="nodump"/>
        <rom name="questionable.bin" size="256" crc="deadbeef" sha1="0000000000000000000000000000000000000a" status="baddump"/>
      </dataarea>
      <diskarea name="cdrom">
        <disk name="game1cd" sha1="1111111111111111111111111111111111111a" status="good" writeable="no"/>
        <disk name="savedata" sha1="2222222222222222222222222222222222222b" status="good" writeable="yes"/>
      </diskarea>
    </part>
  </software>
</softwarelist>
"#;

    #[test]
    fn softwarelist_imports_normal_entries_and_excludes_per_10_18() {
        let dir = tempfile::tempdir().unwrap();
        let xml_path = write_xml(dir.path(), "softlist.xml", SOFTWARELIST_XML);

        let mut conn = open_in_memory().unwrap();
        let summary = import_mame_reference_set(
            &mut conn,
            MameFormat::SoftwareList,
            &xml_path,
            "example softlist",
            &options(false, MergeMode::Split),
            now(),
        )
        .unwrap();

        // Included: game1.bin, game1cd. Excluded: reload/fill (loadflag),
        // nodump, baddump (default), the writeable disk. 5 excluded, 2 imported.
        assert_eq!(summary.imported_count, 2);
        assert_eq!(summary.excluded_count, 5);

        let files = repo::list_reference_files(&conn, summary.reference_set_id).unwrap();
        let by_path: HashMap<&str, &repo::ReferenceFileRow> =
            files.iter().map(|f| (f.path.as_str(), f)).collect();

        let rom = by_path
            .get("game1.zip/game1.bin")
            .expect("rom entry imported");
        assert_eq!(rom.size, 65536);
        let crc32: Option<u32> = conn
            .query_row(
                "SELECT crc32 FROM reference_file WHERE id = ?1",
                [rom.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(crc32, Some(0x1234_5678));

        assert!(by_path.contains_key("game1.zip/game1cd"));
        assert!(
            !by_path.contains_key("game1.zip/savedata"),
            "writeable disk must be excluded"
        );
        assert!(!by_path.contains_key("game1.zip/reload_part.bin"));
        assert!(!by_path.contains_key("game1.zip/fill_byte.bin"));
        assert!(!by_path.contains_key("game1.zip/broken.bin"));
        assert!(!by_path.contains_key("game1.zip/questionable.bin"));
    }

    #[test]
    fn softwarelist_include_baddump_option_includes_it() {
        let dir = tempfile::tempdir().unwrap();
        let xml_path = write_xml(dir.path(), "softlist.xml", SOFTWARELIST_XML);

        let mut conn = open_in_memory().unwrap();
        let summary = import_mame_reference_set(
            &mut conn,
            MameFormat::SoftwareList,
            &xml_path,
            "example softlist",
            &options(true, MergeMode::Split),
            now(),
        )
        .unwrap();

        assert_eq!(summary.imported_count, 3);
        let files = repo::list_reference_files(&conn, summary.reference_set_id).unwrap();
        assert!(files.iter().any(|f| f.path == "game1.zip/questionable.bin"));
    }

    /// Golden sample shaped after the mapping doc's mame.dtd walkthrough: a parent
    /// machine, a clone sharing a ROM via merge/romof, a bios-select ROM, an optional
    /// ROM, and a device-only sub-machine.
    const MACHINELIST_XML: &str = r#"<?xml version="1.0"?>
<mame build="0.001">
  <machine name="parent" sourcefile="parent.cpp">
    <description>Parent Game</description>
    <rom name="parent.bin" size="1024" crc="11111111" sha1="1111111111111111111111111111111111111a" status="good"/>
    <rom name="bios_a.bin" size="16" crc="22222222" bios="bios_a" status="good"/>
    <rom name="extra.bin" size="8" crc="33333333" optional="yes" status="good"/>
  </machine>
  <machine name="clone" romof="parent" cloneof="parent" sourcefile="parent.cpp">
    <description>Parent Game (clone)</description>
    <rom name="parent.bin" merge="parent.bin" size="1024" crc="11111111" status="good"/>
    <rom name="clone_only.bin" size="512" crc="44444444" sha1="2222222222222222222222222222222222222b" status="good"/>
  </machine>
  <machine name="subdevice" isdevice="yes" sourcefile="device.cpp">
    <description>Internal device</description>
    <rom name="device.bin" size="4" crc="55555555" status="good"/>
  </machine>
</mame>
"#;

    #[test]
    fn machinelist_split_mode_ignores_merge_and_keeps_each_machines_own_copy() {
        let dir = tempfile::tempdir().unwrap();
        let xml_path = write_xml(dir.path(), "mame.xml", MACHINELIST_XML);

        let mut conn = open_in_memory().unwrap();
        let summary = import_mame_reference_set(
            &mut conn,
            MameFormat::MachineList,
            &xml_path,
            "example mamelist",
            &options(false, MergeMode::Split),
            now(),
        )
        .unwrap();

        let files = repo::list_reference_files(&conn, summary.reference_set_id).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();

        // Split mode: clone keeps its own literal copy of parent.bin under its own zip.
        assert!(paths.contains(&"parent.zip/parent.bin"));
        assert!(paths.contains(&"clone.zip/parent.bin"));
        assert!(paths.contains(&"clone.zip/clone_only.bin"));

        // Excluded regardless of merge mode: bios-select, optional, and the isdevice
        // sub-machine's rom.
        assert!(!paths.contains(&"parent.zip/bios_a.bin"));
        assert!(!paths.contains(&"parent.zip/extra.bin"));
        assert!(!paths.iter().any(|p| p.starts_with("subdevice.zip/")));
    }

    #[test]
    fn machinelist_merged_mode_resolves_merge_and_romof_to_the_parent_archive() {
        let dir = tempfile::tempdir().unwrap();
        let xml_path = write_xml(dir.path(), "mame.xml", MACHINELIST_XML);

        let mut conn = open_in_memory().unwrap();
        let summary = import_mame_reference_set(
            &mut conn,
            MameFormat::MachineList,
            &xml_path,
            "example mamelist",
            &options(false, MergeMode::Merged),
            now(),
        )
        .unwrap();

        let files = repo::list_reference_files(&conn, summary.reference_set_id).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();

        // Merged mode: the clone's merge="parent.bin" + romof="parent" resolves to the
        // *parent's* archive — the clone's own zip does not get a redundant copy.
        assert!(paths.contains(&"parent.zip/parent.bin"));
        assert!(!paths.contains(&"clone.zip/parent.bin"));
        // A rom without a merge attribute is unaffected by merge mode.
        assert!(paths.contains(&"clone.zip/clone_only.bin"));

        // Exactly 2 rows: parent.bin (the parent machine's own entry — the clone's
        // merge-resolved duplicate of the same path is dropped, not double-inserted)
        // and clone_only.bin. bios_a/extra/device.bin stay excluded regardless of mode.
        assert_eq!(summary.imported_count, 2);
        assert_eq!(files.len(), 2);
    }
}
