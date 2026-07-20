//! An in-memory `sqlite-vec` mirror of the on-disk `embeddings` table —
//! ML-SPEC.md §2's answer to a real, measured latency failure
//! (`workplan/research/sqlite-vec-100k-benchmark.md`: 352.8ms median /
//! 376.7ms p95 on-disk vs. 94.5ms median in-memory, at 100k×768-dim — same
//! brute-force distance math, the gap is on-disk file I/O). The on-disk
//! `embeddings` table stays the durable source of truth; this mirror is
//! rebuilt from scratch at library-open (never itself persisted), and every
//! new embedding write goes to both ([`VecMirror::upsert`] alongside
//! [`crate::upsert_embedding`]).

use std::sync::Once;

use rusqlite::{Connection, params};

static REGISTER_VEC_EXTENSION: Once = Once::new();

/// Registers `sqlite-vec`'s `vec0` virtual table type on every connection
/// this process opens from here on — `sqlite3_auto_extension` is global
/// and process-wide, with no "unregister" call, which is fine here since
/// this whole process only ever wants `vec0` available.
///
/// # Safety
/// `sqlite3_auto_extension` is `sqlite-vec`'s own documented registration
/// entry point (its crate-level doc test uses this exact
/// transmute-a-function-pointer pattern) — no safe wrapper exists in the
/// `sqlite-vec` crate itself. `Once` only protects the FFI call from
/// racing across threads, not anything about the extension's own
/// thread-safety, which is `sqlite-vec`'s contract to uphold, not this
/// function's.
#[allow(unsafe_code)]
fn ensure_vec_extension_registered() {
    // Matches `sqlite-vec`'s own crate-level test verbatim (its only
    // documented registration example) rather than inventing an explicit
    // transmute target type — the exact `Option<F>` signature
    // `sqlite3_auto_extension` expects is `rusqlite::ffi`'s internal
    // detail, and matching their tested call shape is the safer bet.
    REGISTER_VEC_EXTENSION.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ())));
    });
}

pub struct VecMirror {
    conn: Connection,
}

impl VecMirror {
    /// Builds a fresh in-memory `vec0` mirror and loads every embedding
    /// `source` has for `model_id` — the "at library-open" step (§2/§9).
    pub fn build(source: &Connection, model_id: i64, dimension: usize) -> rusqlite::Result<Self> {
        ensure_vec_extension_registered();
        let conn = Connection::open_in_memory()?;
        conn.execute(&format!("CREATE VIRTUAL TABLE vec_mirror USING vec0(embedding float[{dimension}])"), [])?;

        let mut stmt = source.prepare("SELECT image_id, vector FROM embeddings WHERE model_id = ?1")?;
        let rows = stmt.query_map([model_id], |row| {
            let image_id: i64 = row.get(0)?;
            let vector: Vec<u8> = row.get(1)?;
            Ok((image_id, vector))
        })?;
        let mirror = Self { conn };
        for row in rows {
            let (image_id, vector) = row?;
            mirror.insert_row(image_id, &vector)?;
        }
        Ok(mirror)
    }

    fn insert_row(&self, image_id: i64, vector: &[u8]) -> rusqlite::Result<()> {
        self.conn.execute("INSERT INTO vec_mirror (rowid, embedding) VALUES (?1, ?2)", params![image_id, vector])?;
        Ok(())
    }

    /// Inserts or replaces one embedding in the live mirror — the "new
    /// embeddings write to both" half of §2. Delete-then-insert rather
    /// than an `ON CONFLICT` upsert: `vec0`'s shadow-table-backed virtual
    /// table doesn't reliably support the same `ON CONFLICT` semantics a
    /// real table does, so this sticks to the operations `vec0` is
    /// documented to support directly.
    pub fn upsert(&self, image_id: i64, vector: &[u8]) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM vec_mirror WHERE rowid = ?1", params![image_id])?;
        self.insert_row(image_id, vector)
    }

    /// The `k` nearest neighbors to `query_vector` (raw little-endian
    /// `f32` bytes, matching [`build`](Self::build)'s `dimension`), by
    /// `image_id`, nearest (smallest L2 distance — `vec0`'s default
    /// metric, matching the benchmark's own methodology) first.
    pub fn query_similar(&self, query_vector: &[u8], k: usize) -> rusqlite::Result<Vec<(i64, f64)>> {
        let mut stmt = self.conn.prepare("SELECT rowid, distance FROM vec_mirror WHERE embedding MATCH ?1 ORDER BY distance LIMIT ?2")?;
        stmt.query_map(params![query_vector, k as i64], |row| Ok((row.get(0)?, row.get(1)?)))?.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector_bytes(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn source_conn_with_embeddings(rows: &[(i64, &[f32])]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE embeddings (image_id INTEGER, model_id INTEGER, vector BLOB)", []).unwrap();
        for (image_id, vector) in rows {
            conn.execute(
                "INSERT INTO embeddings (image_id, model_id, vector) VALUES (?1, 1, ?2)",
                params![image_id, vector_bytes(vector)],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn build_loads_every_stored_embedding_for_the_given_model() {
        let source = source_conn_with_embeddings(&[(1, &[1.0, 0.0, 0.0]), (2, &[0.0, 1.0, 0.0])]);
        let mirror = VecMirror::build(&source, 1, 3).unwrap();

        let results = mirror.query_similar(&vector_bytes(&[1.0, 0.0, 0.0]), 2).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, 1, "the exact match should rank first");
        assert!(results[0].1 < results[1].1, "distances should be ascending");
    }

    #[test]
    fn build_only_loads_embeddings_for_the_requested_model() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE embeddings (image_id INTEGER, model_id INTEGER, vector BLOB)", []).unwrap();
        conn.execute(
            "INSERT INTO embeddings (image_id, model_id, vector) VALUES (1, 1, ?1), (2, 2, ?1)",
            params![vector_bytes(&[1.0, 0.0])],
        )
        .unwrap();

        let mirror = VecMirror::build(&conn, 1, 2).unwrap();
        let results = mirror.query_similar(&vector_bytes(&[1.0, 0.0]), 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 1);
    }

    #[test]
    fn upsert_adds_a_new_row_the_mirror_wasnt_built_with() {
        let source = source_conn_with_embeddings(&[(1, &[1.0, 0.0])]);
        let mirror = VecMirror::build(&source, 1, 2).unwrap();

        mirror.upsert(2, &vector_bytes(&[0.0, 1.0])).unwrap();

        let results = mirror.query_similar(&vector_bytes(&[0.0, 1.0]), 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, 2, "the freshly upserted exact match should rank first");
    }

    #[test]
    fn upsert_replaces_an_existing_rows_vector_rather_than_duplicating() {
        let source = source_conn_with_embeddings(&[(1, &[1.0, 0.0])]);
        let mirror = VecMirror::build(&source, 1, 2).unwrap();

        mirror.upsert(1, &vector_bytes(&[0.0, 1.0])).unwrap();

        let results = mirror.query_similar(&vector_bytes(&[0.0, 1.0]), 10).unwrap();
        assert_eq!(results.len(), 1, "upsert must replace, not duplicate, the row");
        assert_eq!(results[0].0, 1);
        assert!(results[0].1 < 0.001, "the mirror should now hold the replaced vector, not the original");
    }
}
