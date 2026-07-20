//! XMP sidecar read/write and sync orchestration, per workplan/SPEC.md §7.
//!
//! "SQLite is authoritative; every tag/metadata change mirrors to an XMP
//! sidecar next to its managed file." This milestone's metadata surface is
//! the tag list (§10's `tags`/`image_tags`), so the sidecar this crate
//! writes carries exactly that: a standards-conformant `x:xmpmeta` document
//! with the tags encoded as the standard XMP keyword field, `dc:subject`,
//! an `rdf:Bag` of `rdf:li` entries.
//!
//! Real XML — not hand-formatted string concatenation — because tag names
//! can contain `&`, `<`, `>`, or arbitrary Unicode, and because §7's whole
//! point is that XMP is "a real, widely-read standard... not a lossy
//! compromise." `quick_xml` handles escaping and parsing correctly on both
//! the write and read side.
//!
//! **Where sync orchestration lives**: [`sync_sidecar`] reads tags via
//! `lenslocker-catalog` and writes/updates the sidecar file, so this crate
//! depends on `lenslocker-catalog` (not the other way around) — keeping
//! `catalog` pure SQL/no-I/O as it's been since Milestone 0. This also
//! reads naturally as "xmp owns keeping the sidecar in sync," which is the
//! reasonable default the Milestone 4 brief suggested when no better home
//! presented itself.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;
use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, thiserror::Error)]
pub enum XmpError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Xml(#[from] quick_xml::Error),
    #[error(transparent)]
    Encoding(#[from] quick_xml::encoding::EncodingError),
    #[error("malformed XMP sidecar: {0}")]
    Malformed(String),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("image {0} has no stored_path recorded in the catalog")]
    NoStoredPath(i64),
}

/// Writes a minimal, valid `x:xmpmeta`/`rdf:RDF`/`rdf:Description` XMP
/// document to `path`, encoding `tags` as `dc:subject`'s `rdf:Bag` of
/// `rdf:li` entries — the standard XMP keyword field, per §7's requirement
/// that this be a real, widely-read format (readable by Adobe products,
/// digiKam, darktable — the tools §7 names).
pub fn write_sidecar(path: &Path, tags: &[String]) -> Result<(), XmpError> {
    let mut buf = Vec::new();
    let mut writer = Writer::new_with_indent(Cursor::new(&mut buf), b' ', 2);

    writer.write_event(Event::Decl(quick_xml::events::BytesDecl::new(
        "1.0",
        Some("UTF-8"),
        None,
    )))?;

    let mut xmpmeta = BytesStart::new("x:xmpmeta");
    xmpmeta.push_attribute(("xmlns:x", "adobe:ns:meta/"));
    writer.write_event(Event::Start(xmpmeta))?;

    let mut rdf = BytesStart::new("rdf:RDF");
    rdf.push_attribute(("xmlns:rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"));
    writer.write_event(Event::Start(rdf))?;

    let mut description = BytesStart::new("rdf:Description");
    description.push_attribute(("rdf:about", ""));
    description.push_attribute(("xmlns:dc", "http://purl.org/dc/elements/1.1/"));
    writer.write_event(Event::Start(description))?;

    writer.write_event(Event::Start(BytesStart::new("dc:subject")))?;
    writer.write_event(Event::Start(BytesStart::new("rdf:Bag")))?;
    for tag in tags {
        writer.write_event(Event::Start(BytesStart::new("rdf:li")))?;
        writer.write_event(Event::Text(BytesText::new(tag)))?;
        writer.write_event(Event::End(BytesEnd::new("rdf:li")))?;
    }
    writer.write_event(Event::End(BytesEnd::new("rdf:Bag")))?;
    writer.write_event(Event::End(BytesEnd::new("dc:subject")))?;

    writer.write_event(Event::End(BytesEnd::new("rdf:Description")))?;
    writer.write_event(Event::End(BytesEnd::new("rdf:RDF")))?;
    writer.write_event(Event::End(BytesEnd::new("x:xmpmeta")))?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, buf)?;
    Ok(())
}

/// Reads back the tag list from an XMP sidecar written by [`write_sidecar`]
/// (or any other standards-conformant XMP document carrying `dc:subject`'s
/// `rdf:Bag`/`rdf:li` entries) — real parsing, not regex/substring
/// scraping, so it tolerates attribute ordering, whitespace, and other
/// well-formed variations a hand-written matcher would choke on.
pub fn read_sidecar(path: &Path) -> Result<Vec<String>, XmpError> {
    let xml = fs::read_to_string(path)?;
    // Deliberately not `trim_text(true)`: this crate's XML never carries
    // meaningful whitespace outside `<rdf:li>` (only pretty-printing
    // indentation between tags, which is skipped anyway since it's only
    // captured while `in_li` is true), but `trim_text` trims *every* text
    // node's leading/trailing whitespace — including the space characters
    // in a tag's own content, which would corrupt a tag like `"a > b"`
    // (split across `Text`/`GeneralRef`/`Text` events by the `>` escape).
    let mut reader = Reader::from_str(&xml);

    let mut tags = Vec::new();
    let mut in_subject_bag = false;
    let mut in_li = false;
    let mut current = String::new();

    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                let local = local_name(&e.name());
                if local == "subject" {
                    in_subject_bag = true;
                } else if in_subject_bag && local == "li" {
                    in_li = true;
                    current.clear();
                }
            }
            // A tag's text content may contain XML entity/character
            // references (`&amp;`, `&#39;`, …), which quick-xml surfaces as
            // a separate `GeneralRef` event rather than folding into
            // `Text` — both must be appended in document order to
            // reconstruct the original string.
            Event::Text(e) if in_li => {
                current.push_str(&e.decode()?);
            }
            Event::GeneralRef(e) if in_li => {
                current.push_str(&resolve_general_ref(&e.decode()?)?);
            }
            Event::End(e) => {
                let local = local_name(&e.name());
                if in_subject_bag && local == "li" {
                    in_li = false;
                    tags.push(std::mem::take(&mut current));
                } else if local == "subject" {
                    in_subject_bag = false;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(tags)
}

/// Resolves one `&name;`/`&#NN;`/`&#xNN;` general reference (already
/// stripped of its `&`/`;` delimiters by the reader) to its literal string
/// value — the predefined XML entities (`amp`, `lt`, `gt`, `apos`, `quot`)
/// plus numeric character references, which is everything [`write_sidecar`]
/// itself ever produces and everything a standards-conformant XMP writer
/// is expected to emit.
fn resolve_general_ref(name: &str) -> Result<String, XmpError> {
    if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
        let code = u32::from_str_radix(hex, 16).map_err(|_| {
            XmpError::Malformed(format!("invalid hex character reference &#x{hex};"))
        })?;
        return char::from_u32(code)
            .map(String::from)
            .ok_or_else(|| XmpError::Malformed(format!("invalid Unicode code point &#x{hex};")));
    }
    if let Some(dec) = name.strip_prefix('#') {
        let code = dec.parse::<u32>().map_err(|_| {
            XmpError::Malformed(format!("invalid decimal character reference &#{dec};"))
        })?;
        return char::from_u32(code)
            .map(String::from)
            .ok_or_else(|| XmpError::Malformed(format!("invalid Unicode code point &#{dec};")));
    }
    quick_xml::escape::resolve_xml_entity(name)
        .map(str::to_string)
        .ok_or_else(|| XmpError::Malformed(format!("unknown XML entity &{name};")))
}

/// The local (namespace-stripped) part of a qualified XML element name —
/// `dc:subject` and a bare `subject` both compare equal against `"subject"`,
/// so this doesn't care whether a sidecar (ours or a third-party tool's)
/// declared the `dc`/`rdf` prefixes differently.
fn local_name(name: &quick_xml::name::QName) -> String {
    String::from_utf8_lossy(name.local_name().as_ref()).into_owned()
}

/// The sidecar path for a managed file: same directory and basename as its
/// blob, `.xmp` extension — mirrors `lenslocker-import::LibraryPaths`'
/// blob-path convention without depending on that crate (this crate takes
/// `stored_path` from the `images` row instead, which already encodes the
/// exact on-disk location).
fn sidecar_path_for(stored_path: &Path) -> PathBuf {
    stored_path.with_extension("xmp")
}

/// Syncs `image_id`'s sidecar to match its current tags in the catalog:
/// reads tags via `lenslocker-catalog`, writes them to the `.xmp` file next
/// to the image's blob, and updates `images.sidecar_path`/
/// `sidecar_synced_at`. Per §7: "every tag/metadata change mirrors to an
/// XMP sidecar" — this is the one function that makes that true; callers
/// (manual tagging via direct catalog calls, for now) invoke it after every
/// `add_tag`/`remove_tag`.
pub fn sync_sidecar(conn: &Connection, image_id: i64) -> Result<PathBuf, XmpError> {
    let stored_path: Option<String> = conn
        .query_row(
            "SELECT stored_path FROM images WHERE id = ?1",
            [image_id],
            |row| row.get(0),
        )
        .optional()?;
    let stored_path = stored_path.ok_or(XmpError::NoStoredPath(image_id))?;
    let sidecar_path = sidecar_path_for(Path::new(&stored_path));

    let tags = lenslocker_catalog::tags_for_image(conn, image_id)?;
    write_sidecar(&sidecar_path, &tags)?;

    conn.execute(
        "UPDATE images
         SET sidecar_path = ?1, sidecar_synced_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?2",
        rusqlite::params![sidecar_path.to_string_lossy(), image_id],
    )?;

    Ok(sidecar_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writing_and_reading_back_a_tag_list_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.xmp");
        let tags = vec![
            "sunset".to_string(),
            "beach".to_string(),
            "vacation".to_string(),
        ];

        write_sidecar(&path, &tags).unwrap();
        let read_back = read_sidecar(&path).unwrap();

        assert_eq!(read_back, tags);
    }

    #[test]
    fn an_empty_tag_list_round_trips_to_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.xmp");

        write_sidecar(&path, &[]).unwrap();
        let read_back = read_sidecar(&path).unwrap();

        assert!(read_back.is_empty());
    }

    #[test]
    fn tags_with_xml_special_characters_round_trip_safely() {
        // The exact case naive string concatenation would corrupt: `&`,
        // `<`, `>` in a tag name must survive a full write -> read cycle.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.xmp");
        let tags = vec![
            "R&D".to_string(),
            "<classified>".to_string(),
            "a > b".to_string(),
        ];

        write_sidecar(&path, &tags).unwrap();
        let read_back = read_sidecar(&path).unwrap();

        assert_eq!(read_back, tags);

        // Also confirm the file on disk is well-formed XML (a real parse
        // succeeds), proving these characters were actually escaped rather
        // than corrupting the document structure.
        let raw = fs::read_to_string(&path).unwrap();
        let mut reader = Reader::from_str(&raw);
        loop {
            match reader.read_event() {
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(e) => panic!("sidecar is not well-formed XML: {e}"),
            }
        }
    }

    #[test]
    fn tags_with_unicode_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.xmp");
        let tags = vec!["日本語".to_string(), "café".to_string(), "🌅".to_string()];

        write_sidecar(&path, &tags).unwrap();
        let read_back = read_sidecar(&path).unwrap();

        assert_eq!(read_back, tags);
    }

    fn test_conn_with_image(blob_path: &Path, format: &str) -> (Connection, i64) {
        let mut conn = Connection::open_in_memory().unwrap();
        lenslocker_catalog::migrate(&mut conn).unwrap();
        fs::write(blob_path, b"fake blob bytes").unwrap();

        conn.execute(
            "INSERT INTO libraries (name, root_path) VALUES ('lib', 'A:/lib')",
            [],
        )
        .unwrap();
        let library_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO images (
                library_id, original_hash, stored_hash, stored_path,
                original_format, stored_format, file_size_bytes
            ) VALUES (?1, x'00', x'00', ?2, ?3, ?3, 0)",
            rusqlite::params![library_id, blob_path.to_string_lossy(), format],
        )
        .unwrap();
        let image_id = conn.last_insert_rowid();
        (conn, image_id)
    }

    #[test]
    fn sync_sidecar_writes_current_tags_and_updates_catalog_state() {
        let dir = tempfile::tempdir().unwrap();
        let blob_path = dir.path().join("blob.jxl");
        let (conn, image_id) = test_conn_with_image(&blob_path, "jxl");

        lenslocker_catalog::add_tag(&conn, image_id, "sunset").unwrap();
        lenslocker_catalog::add_tag(&conn, image_id, "beach").unwrap();

        let sidecar_path = sync_sidecar(&conn, image_id).unwrap();

        assert_eq!(sidecar_path, blob_path.with_extension("xmp"));
        assert!(sidecar_path.exists());
        assert_eq!(
            read_sidecar(&sidecar_path).unwrap(),
            vec!["beach".to_string(), "sunset".to_string()]
        );

        let (db_sidecar_path, synced_at): (String, Option<String>) = conn
            .query_row(
                "SELECT sidecar_path, sidecar_synced_at FROM images WHERE id = ?1",
                [image_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(db_sidecar_path, sidecar_path.to_string_lossy());
        assert!(synced_at.is_some());
    }

    #[test]
    fn removing_a_tag_and_resyncing_changes_the_sidecar_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let blob_path = dir.path().join("blob.webp");
        let (conn, image_id) = test_conn_with_image(&blob_path, "webp");

        lenslocker_catalog::add_tag(&conn, image_id, "keep").unwrap();
        lenslocker_catalog::add_tag(&conn, image_id, "drop").unwrap();
        let sidecar_path = sync_sidecar(&conn, image_id).unwrap();
        assert_eq!(
            read_sidecar(&sidecar_path).unwrap(),
            vec!["drop".to_string(), "keep".to_string()]
        );

        lenslocker_catalog::remove_tag(&conn, image_id, "drop").unwrap();
        sync_sidecar(&conn, image_id).unwrap();

        assert_eq!(
            read_sidecar(&sidecar_path).unwrap(),
            vec!["keep".to_string()]
        );
    }
}
