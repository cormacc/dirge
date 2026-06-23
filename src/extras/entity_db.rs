//! SQLite-backed entity/relation graph storage (dirge-graph-search, #393).
//!
//! Two tables added by migration v14: `entities` (kind + name rows
//! extracted from tool output by Janet compressors) and `relations`
//! (typed edges connecting entities). FTS5 is standalone (app-managed
//! sync, no triggers) matching the v7 memories_fts pattern.
//!
//! All functions are gated behind `#[cfg(feature = "experimental-graph-search")]`
//! — if the feature is never enabled, none of this compiles and migration
//! v14 never runs.

use rusqlite::{Connection, params};

use crate::extras::fts;
use crate::extras::session_db::redact_for_fts;

/// Insert a new entity row. Returns the new row's id.
///
/// FTS5 is synced after insert.
pub fn insert_entity(
    conn: &Connection,
    session_id: &str,
    message_id: Option<i64>,
    kind: &str,
    name: &str,
    extra: Option<&str>,
) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO entities (session_id, message_id, kind, name, extra) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![session_id, message_id, kind, name, extra],
    )
    .map_err(|e| format!("insert_entity: {e}"))?;

    let id = conn.last_insert_rowid();
    sync_entity_fts(conn, id, name, kind)?;
    Ok(id)
}

/// Insert or skip a duplicate entity by (session_id, kind, name).
/// Updates `extra` if the row already existed.
/// Returns the entity's id (existing or new), and syncs FTS5.
pub fn upsert_entity(
    conn: &Connection,
    session_id: &str,
    message_id: Option<i64>,
    kind: &str,
    name: &str,
    extra: Option<&str>,
) -> Result<i64, String> {
    conn.execute(
        "INSERT OR IGNORE INTO entities (session_id, message_id, kind, name, extra) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![session_id, message_id, kind, name, extra],
    )
    .map_err(|e| format!("upsert_entity insert: {e}"))?;

    let id: i64 = conn
        .query_row(
            "SELECT id FROM entities WHERE session_id = ?1 AND kind = ?2 AND name = ?3",
            params![session_id, kind, name],
            |row| row.get(0),
        )
        .map_err(|e| format!("upsert_entity select: {e}"))?;

    // Update extra if provided and different
    if let Some(extra_val) = extra {
        let _ = conn.execute(
            "UPDATE entities SET extra = ?1 WHERE id = ?2 AND extra IS NOT ?1",
            params![extra_val, id],
        );
    }

    sync_entity_fts(conn, id, name, kind)?;
    Ok(id)
}

/// Insert a typed relation between two entities.
pub fn insert_relation(
    conn: &Connection,
    source_id: i64,
    target_id: i64,
    rel_type: &str,
    session_id: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO relations (source_id, target_id, rel_type, session_id) VALUES (?1, ?2, ?3, ?4)",
        params![source_id, target_id, rel_type, session_id],
    )
    .map_err(|e| format!("insert_relation: {e}"))?;
    Ok(())
}

/// Convenience: upsert two entities and insert a relation between them.
/// The compressor says "error E0308 occurred_in crate dirge-core" — this
/// creates both entities and the edge in one call.
pub fn record_entity_pair(
    conn: &Connection,
    session_id: &str,
    message_id: Option<i64>,
    source_kind: &str,
    source_name: &str,
    target_kind: &str,
    target_name: &str,
    rel_type: &str,
) -> Result<(), String> {
    let sid = upsert_entity(conn, session_id, message_id, source_kind, source_name, None)?;
    let tid = upsert_entity(conn, session_id, message_id, target_kind, target_name, None)?;
    insert_relation(conn, sid, tid, rel_type, session_id)
}

/// FTS5 search over entities by name + kind. Follows memory_db's
/// fts::quote_terms + MATCH pattern. Returns (id, session_id, kind, name, extra, created_at).
pub fn search_entities(
    conn: &Connection,
    query: &str,
    kind_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<(i64, String, String, String, Option<String>, String)>, String> {
    let fts_query = fts::quote_terms(query);
    if fts_query.is_empty() {
        return Ok(Vec::new());
    }

    let sql = if kind_filter.is_some() {
        "SELECT e.id, e.session_id, e.kind, e.name, e.extra, e.created_at
         FROM entities_fts
         JOIN entities e ON e.id = entities_fts.rowid
         WHERE entities_fts MATCH ?1 AND e.kind = ?2
         ORDER BY rank
         LIMIT ?3"
    } else {
        "SELECT e.id, e.session_id, e.kind, e.name, e.extra, e.created_at
         FROM entities_fts
         JOIN entities e ON e.id = entities_fts.rowid
         WHERE entities_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2"
    };

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("search_entities: {e}"))?;

    let rows = if let Some(kind) = kind_filter {
        stmt.query_map(params![fts_query, kind, limit as i64], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .map_err(|e| format!("search_entities query: {e}"))?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>()
    } else {
        stmt.query_map(params![fts_query, limit as i64], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .map_err(|e| format!("search_entities query: {e}"))?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>()
    };

    Ok(rows)
}

// ── Internal helpers ──────────────────────────────────────────────────────

fn sync_entity_fts(conn: &Connection, rowid: i64, name: &str, kind: &str) -> Result<(), String> {
    conn.execute("DELETE FROM entities_fts WHERE rowid = ?1", params![rowid])
        .map_err(|e| format!("entity FTS delete: {e}"))?;
    conn.execute(
        "INSERT INTO entities_fts(rowid, name, kind) VALUES (?1, ?2, ?3)",
        params![rowid, redact_for_fts(name), redact_for_fts(kind)],
    )
    .map_err(|e| format!("entity FTS insert: {e}"))?;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();

        conn.execute_batch(
            "
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL DEFAULT 'cli',
                model TEXT NOT NULL DEFAULT '',
                provider TEXT NOT NULL DEFAULT '',
                started_at TEXT NOT NULL,
                last_active TEXT NOT NULL,
                title TEXT NOT NULL DEFAULT '',
                message_count INTEGER NOT NULL DEFAULT 0,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                role TEXT NOT NULL,
                content TEXT NOT NULL DEFAULT '',
                tool_name TEXT,
                tool_calls TEXT,
                tool_call_id TEXT,
                timestamp TEXT NOT NULL
            );
            CREATE TABLE entities (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                message_id INTEGER REFERENCES messages(id),
                kind TEXT NOT NULL,
                name TEXT NOT NULL,
                extra TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE relations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source_id INTEGER NOT NULL REFERENCES entities(id),
                target_id INTEGER NOT NULL REFERENCES entities(id),
                rel_type TEXT NOT NULL,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                confidence REAL DEFAULT 1.0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE VIRTUAL TABLE entities_fts USING fts5(
                name, kind,
                tokenize='unicode61'
            );
            ",
        )
        .unwrap();

        conn.execute(
            "INSERT INTO sessions (id, started_at, last_active) VALUES ('test-session', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (session_id, role, content, timestamp) VALUES ('test-session', 'tool', '', datetime('now'))",
            [],
        )
        .unwrap();

        conn
    }

    #[test]
    fn insert_and_query_entity() {
        let conn = in_memory_db();
        let id = insert_entity(
            &conn,
            "test-session",
            Some(1),
            "file",
            "src/main.rs",
            Some("modified"),
        )
        .unwrap();

        let row: (String, String) = conn
            .query_row(
                "SELECT kind, name FROM entities WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(row.0, "file");
        assert_eq!(row.1, "src/main.rs");

        // FTS5 should find it
        let results = search_entities(&conn, "main.rs", None, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, "test-session");
        assert_eq!(results[0].3, "src/main.rs");
    }

    #[test]
    fn upsert_entity_dedup() {
        let conn = in_memory_db();
        let id1 = upsert_entity(&conn, "test-session", Some(1), "error", "E0308", None).unwrap();
        let id2 = upsert_entity(
            &conn,
            "test-session",
            Some(1),
            "error",
            "E0308",
            Some("msg"),
        )
        .unwrap();

        assert_eq!(id1, id2, "same (session, kind, name) returns same id");

        // Verify extra was updated
        let extra: Option<String> = conn
            .query_row(
                "SELECT extra FROM entities WHERE id = ?1",
                params![id1],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(extra, Some("msg".to_string()));
    }

    #[test]
    fn insert_relation_and_pair() {
        let conn = in_memory_db();
        let file_id =
            insert_entity(&conn, "test-session", Some(1), "file", "src/main.rs", None).unwrap();
        let err_id = insert_entity(&conn, "test-session", Some(1), "error", "E0308", None).unwrap();

        insert_relation(&conn, err_id, file_id, "occurred_in", "test-session").unwrap();

        // Verify relation exists
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM relations WHERE source_id = ?1 AND target_id = ?2",
                params![err_id, file_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn record_entity_pair_creates_both() {
        let conn = in_memory_db();
        record_entity_pair(
            &conn,
            "test-session",
            Some(1),
            "error",
            "E0308",
            "file",
            "src/main.rs",
            "occurred_in",
        )
        .unwrap();

        // Both entities exist
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);

        // Relation exists
        let rel_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM relations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rel_count, 1);
    }

    #[test]
    fn fts_search_by_kind() {
        let conn = in_memory_db();
        insert_entity(&conn, "test-session", Some(1), "file", "src/main.rs", None).unwrap();
        insert_entity(&conn, "test-session", Some(1), "error", "E0308", None).unwrap();
        insert_entity(&conn, "test-session", Some(1), "file", "src/lib.rs", None).unwrap();

        let files = search_entities(&conn, "src", Some("file"), 10).unwrap();
        assert_eq!(files.len(), 2);
        for f in &files {
            assert_eq!(f.2, "file");
        }

        let errors = search_entities(&conn, "E0308", Some("error"), 10).unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].2, "error");
    }

    #[test]
    fn upsert_different_sessions_independent() {
        let conn = in_memory_db();

        // Add a second session
        conn.execute(
            "INSERT INTO sessions (id, started_at, last_active) VALUES ('other-session', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (session_id, role, content, timestamp) VALUES ('other-session', 'tool', '', datetime('now'))",
            [],
        )
        .unwrap();

        let id1 =
            upsert_entity(&conn, "test-session", Some(1), "file", "src/main.rs", None).unwrap();
        let id2 =
            upsert_entity(&conn, "other-session", Some(2), "file", "src/main.rs", None).unwrap();

        assert_ne!(id1, id2, "different sessions get different rows");
    }
}
