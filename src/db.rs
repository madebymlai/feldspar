use tracing::warn;
use tokio_rusqlite::Connection;

/// Trace data returned by find_traces_by_ids — what Claude sees in pattern recall.
/// Derives Serialize because it's included in WireResponse (Agent 2 needs this).
#[derive(Debug, serde::Serialize)]
pub struct PatternMatch {
    pub trace_id: String,
    pub thinking_mode: Option<String>,
    pub components: Vec<String>,
    pub trust_score: Option<f64>,
    pub trust_reason: Option<String>,
}

/// Trace data for ML bulk training on startup.
pub struct TraceRow {
    pub id: String,
    pub thinking_mode: Option<String>,
    pub components: Vec<String>,
    pub trust_score: Option<f64>,
    pub created_at: i64,
}

/// Leaf node entry for in-memory cache.
pub struct LeafEntry {
    pub trace_id: String,
    pub leaf_nodes: Vec<usize>,
}

pub struct Db {
    conn: Connection,
}

const SCHEMA_SQL: &str = "
    CREATE TABLE IF NOT EXISTS thoughts (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        trace_id        TEXT NOT NULL,
        thought_number  INTEGER NOT NULL,
        thinking_mode   TEXT,
        input_json      TEXT NOT NULL,
        result_json     TEXT NOT NULL,
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_thoughts_trace_id ON thoughts(trace_id);
    CREATE TABLE IF NOT EXISTS traces (
        id              TEXT PRIMARY KEY,
        thinking_mode   TEXT,
        components      TEXT,
        trust_score     REAL,
        trust_reason    TEXT,
        ar_scores       TEXT,
        leaf_nodes      BLOB,
        created_at      INTEGER NOT NULL
    );
";

impl Db {
    pub async fn open(path: &str) -> Option<Self> {
        let conn = match Connection::open(path).await {
            Ok(c) => c,
            Err(e) => {
                warn!("failed to open DB '{}': {}", path, e);
                return None;
            }
        };

        if let Err(e) = conn.call(|conn| {
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| {
                row.get::<_, String>(0)
            })?;
            conn.execute_batch(SCHEMA_SQL)?;
            Ok(())
        }).await {
            warn!("failed to init DB schema: {}", e);
            return None;
        }

        Some(Self { conn })
    }

    pub async fn write_thought(
        &self,
        trace_id: &str,
        thought_number: u32,
        thinking_mode: Option<&str>,
        input_json: &str,
        result_json: &str,
        created_at: i64,
    ) {
        let trace_id = trace_id.to_owned();
        let thinking_mode = thinking_mode.map(|s| s.to_owned());
        let input_json = input_json.to_owned();
        let result_json = result_json.to_owned();

        if let Err(e) = self.conn.call(move |conn| {
            conn.execute(
                "INSERT INTO thoughts (trace_id, thought_number, thinking_mode, input_json, result_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![trace_id, thought_number, thinking_mode, input_json, result_json, created_at],
            )?;
            Ok(())
        }).await {
            warn!("write_thought failed: {}", e);
        }
    }

    pub async fn flush_trace(
        &self,
        trace_id: &str,
        thinking_mode: Option<&str>,
        components: &[String],
        created_at: i64,
    ) {
        let trace_id = trace_id.to_owned();
        let thinking_mode = thinking_mode.map(|s| s.to_owned());
        let components_json = serde_json::to_string(components).unwrap_or_default();

        if let Err(e) = self.conn.call(move |conn| {
            conn.execute(
                "INSERT INTO traces (id, thinking_mode, components, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![trace_id, thinking_mode, components_json, created_at],
            )?;
            Ok(())
        }).await {
            warn!("flush_trace failed: {}", e);
        }
    }

    pub async fn update_trust(&self, trace_id: &str, score: f64, reason: &str) {
        let trace_id = trace_id.to_owned();
        let reason = reason.to_owned();

        if let Err(e) = self.conn.call(move |conn| {
            conn.execute(
                "UPDATE traces SET trust_score = ?1, trust_reason = ?2 WHERE id = ?3",
                rusqlite::params![score, reason, trace_id],
            )?;
            Ok(())
        }).await {
            warn!("update_trust failed: {}", e);
        }
    }

    pub async fn update_ar(&self, trace_id: &str, ar_scores_json: &str) {
        let trace_id = trace_id.to_owned();
        let ar_scores_json = ar_scores_json.to_owned();

        if let Err(e) = self.conn.call(move |conn| {
            conn.execute(
                "UPDATE traces SET ar_scores = ?1 WHERE id = ?2",
                rusqlite::params![ar_scores_json, trace_id],
            )?;
            Ok(())
        }).await {
            warn!("update_ar failed: {}", e);
        }
    }

    pub async fn store_leaf_nodes(&self, trace_id: &str, leaf_nodes: &[usize]) {
        let trace_id = trace_id.to_owned();
        let blob = match bincode::encode_to_vec(leaf_nodes, bincode::config::standard()) {
            Ok(b) => b,
            Err(e) => {
                warn!("store_leaf_nodes encode failed for {}: {}", trace_id, e);
                return;
            }
        };

        if let Err(e) = self.conn.call(move |conn| {
            conn.execute(
                "UPDATE traces SET leaf_nodes = ?1 WHERE id = ?2",
                rusqlite::params![blob, trace_id],
            )?;
            Ok(())
        }).await {
            warn!("store_leaf_nodes failed: {}", e);
        }
    }

    pub async fn load_traces(&self) -> Vec<TraceRow> {
        self.conn.call(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, thinking_mode, components, trust_score, created_at FROM traces"
            )?;
            let rows = stmt.query_map([], |row| {
                let components_json: Option<String> = row.get(2)?;
                let components: Vec<String> = components_json
                    .and_then(|j| serde_json::from_str(&j).ok())
                    .unwrap_or_default();
                Ok(TraceRow {
                    id: row.get(0)?,
                    thinking_mode: row.get(1)?,
                    components,
                    trust_score: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        }).await.unwrap_or_else(|e| {
            warn!("load_traces failed: {}", e);
            Vec::new()
        })
    }

    pub async fn load_leaf_nodes(&self) -> Vec<LeafEntry> {
        self.conn.call(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, leaf_nodes FROM traces WHERE leaf_nodes IS NOT NULL"
            )?;
            let mut entries = Vec::new();
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?;
            for row in rows {
                let (trace_id, blob) = row?;
                match bincode::decode_from_slice::<Vec<usize>, _>(
                    &blob, bincode::config::standard()
                ) {
                    Ok((leaf_nodes, _)) => entries.push(LeafEntry { trace_id, leaf_nodes }),
                    Err(e) => warn!("skipping trace {}: leaf_nodes decode failed: {}", trace_id, e),
                }
            }
            Ok(entries)
        }).await.unwrap_or_else(|e| {
            warn!("load_leaf_nodes failed: {}", e);
            Vec::new()
        })
    }

    pub async fn find_traces_by_ids(&self, ids: &[String]) -> Vec<PatternMatch> {
        if ids.is_empty() {
            return Vec::new();
        }
        let ids = ids.to_vec();

        self.conn.call(move |conn| {
            let placeholders: String = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT id, thinking_mode, components, trust_score, trust_reason
                 FROM traces WHERE id IN ({})",
                placeholders
            );
            let mut stmt = conn.prepare(&sql)?;
            let params: Vec<&dyn rusqlite::types::ToSql> = ids
                .iter()
                .map(|s| s as &dyn rusqlite::types::ToSql)
                .collect();
            let rows = stmt.query_map(params.as_slice(), |row| {
                let components_json: Option<String> = row.get(2)?;
                let components: Vec<String> = components_json
                    .and_then(|j| serde_json::from_str(&j).ok())
                    .unwrap_or_default();
                Ok(PatternMatch {
                    trace_id: row.get(0)?,
                    thinking_mode: row.get(1)?,
                    components,
                    trust_score: row.get(3)?,
                    trust_reason: row.get(4)?,
                })
            })?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        }).await.unwrap_or_else(|e| {
            warn!("find_traces_by_ids failed: {}", e);
            Vec::new()
        })
    }

    pub async fn prune(&self, trace_ids: &[String]) {
        if trace_ids.is_empty() {
            return;
        }
        let ids = trace_ids.to_vec();

        if let Err(e) = self.conn.call(move |conn| {
            let placeholders: String = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let params: Vec<&dyn rusqlite::types::ToSql> = ids
                .iter()
                .map(|s| s as &dyn rusqlite::types::ToSql)
                .collect();
            conn.execute_batch("BEGIN")?;
            let r1 = conn.execute(
                &format!("DELETE FROM thoughts WHERE trace_id IN ({})", placeholders),
                params.as_slice(),
            );
            let r2 = conn.execute(
                &format!("DELETE FROM traces WHERE id IN ({})", placeholders),
                params.as_slice(),
            );
            if r1.is_err() || r2.is_err() {
                conn.execute_batch("ROLLBACK")?;
                r1?;
                r2?;
            } else {
                conn.execute_batch("COMMIT")?;
            }
            Ok(())
        }).await {
            warn!("prune failed: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    async fn test_db() -> (Db, NamedTempFile) {
        let f = NamedTempFile::new().expect("tempfile");
        let path = f.path().to_str().expect("path").to_owned();
        let db = Db::open(&path).await.expect("Db::open");
        (db, f)
    }

    // ─── open ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_open_creates_file() {
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_str().unwrap().to_owned();
        let db = Db::open(&path).await;
        assert!(db.is_some());
        assert!(std::path::Path::new(&path).exists());
    }

    #[tokio::test]
    async fn test_open_wal_mode() {
        let (db, _f) = test_db().await;
        let mode: String = db.conn.call(|conn| {
            Ok(conn.pragma_query_value(None, "journal_mode", |r| r.get(0))?)
        }).await.unwrap();
        assert_eq!(mode, "wal");
    }

    #[tokio::test]
    async fn test_open_idempotent() {
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_str().unwrap().to_owned();
        let db1 = Db::open(&path).await;
        let db2 = Db::open(&path).await;
        assert!(db1.is_some());
        assert!(db2.is_some());
    }

    #[tokio::test]
    async fn test_open_bad_path_returns_none() {
        let db = Db::open("/nonexistent/dir/test.db").await;
        assert!(db.is_none());
    }

    // ─── write_thought ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_write_thought() {
        let (db, _f) = test_db().await;
        db.write_thought("trace-1", 1, Some("debugging"), r#"{"x":1}"#, r#"{"y":2}"#, 1000).await;

        let count: i64 = db.conn.call(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM thoughts WHERE trace_id = 'trace-1'",
                [],
                |r| r.get(0),
            )?)
        }).await.unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_write_thought_multiple() {
        let (db, _f) = test_db().await;
        for i in 1..=5 {
            db.write_thought("trace-x", i, None, "{}", "{}", i as i64 * 100).await;
        }
        let count: i64 = db.conn.call(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM thoughts WHERE trace_id = 'trace-x'",
                [],
                |r| r.get(0),
            )?)
        }).await.unwrap();
        assert_eq!(count, 5);
    }

    // ─── flush_trace ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_flush_trace() {
        let (db, _f) = test_db().await;
        db.flush_trace("t1", Some("architecture"), &["auth".to_string()], 999).await;

        let (trust, ar, leaf): (Option<f64>, Option<String>, Option<Vec<u8>>) =
            db.conn.call(|conn| {
                Ok(conn.query_row(
                    "SELECT trust_score, ar_scores, leaf_nodes FROM traces WHERE id = 't1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            }).await.unwrap();

        assert!(trust.is_none());
        assert!(ar.is_none());
        assert!(leaf.is_none());
    }

    // ─── update_trust ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_update_trust() {
        let (db, _f) = test_db().await;
        db.flush_trace("t2", None, &[], 1).await;
        db.update_trust("t2", 7.5, "good reasoning").await;

        let (score, reason): (f64, String) = db.conn.call(|conn| {
            Ok(conn.query_row(
                "SELECT trust_score, trust_reason FROM traces WHERE id = 't2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        }).await.unwrap();

        assert!((score - 7.5).abs() < f64::EPSILON);
        assert_eq!(reason, "good reasoning");
    }

    // ─── update_ar ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_update_ar() {
        let (db, _f) = test_db().await;
        db.flush_trace("t3", None, &[], 1).await;
        db.update_ar("t3", r#"{"critical":0,"recommended":2}"#).await;

        let ar: String = db.conn.call(|conn| {
            Ok(conn.query_row(
                "SELECT ar_scores FROM traces WHERE id = 't3'",
                [],
                |r| r.get(0),
            )?)
        }).await.unwrap();

        assert_eq!(ar, r#"{"critical":0,"recommended":2}"#);
    }

    // ─── store_leaf_nodes ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_store_leaf_nodes() {
        let (db, _f) = test_db().await;
        db.flush_trace("t4", None, &[], 1).await;
        db.store_leaf_nodes("t4", &[1, 5, 42, 100]).await;

        let blob: Vec<u8> = db.conn.call(|conn| {
            Ok(conn.query_row(
                "SELECT leaf_nodes FROM traces WHERE id = 't4'",
                [],
                |r| r.get(0),
            )?)
        }).await.unwrap();

        let (nodes, _): (Vec<usize>, _) =
            bincode::decode_from_slice(&blob, bincode::config::standard()).unwrap();
        assert_eq!(nodes, vec![1, 5, 42, 100]);
    }

    // ─── load_traces ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_load_traces_empty() {
        let (db, _f) = test_db().await;
        assert!(db.load_traces().await.is_empty());
    }

    #[tokio::test]
    async fn test_load_traces_populated() {
        let (db, _f) = test_db().await;
        db.flush_trace("a", Some("debugging"), &["redis".into()], 1).await;
        db.flush_trace("b", Some("architecture"), &["auth".into(), "db".into()], 2).await;
        db.flush_trace("c", None, &[], 3).await;

        let rows = db.load_traces().await;
        assert_eq!(rows.len(), 3);
    }

    // ─── load_leaf_nodes ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_load_leaf_nodes_skips_null() {
        let (db, _f) = test_db().await;
        db.flush_trace("la", None, &[], 1).await;
        db.flush_trace("lb", None, &[], 2).await;
        db.flush_trace("lc", None, &[], 3).await;
        db.store_leaf_nodes("lb", &[7, 8, 9]).await;

        let entries = db.load_leaf_nodes().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].trace_id, "lb");
    }

    #[tokio::test]
    async fn test_load_leaf_nodes_bincode_roundtrip() {
        let (db, _f) = test_db().await;
        db.flush_trace("lr", None, &[], 1).await;
        db.store_leaf_nodes("lr", &[10, 20, 30, 40, 50]).await;

        let entries = db.load_leaf_nodes().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].leaf_nodes, vec![10, 20, 30, 40, 50]);
    }

    #[tokio::test]
    async fn test_load_leaf_nodes_skips_corrupted() {
        let (db, _f) = test_db().await;
        db.flush_trace("lx", None, &[], 1).await;
        // Manually insert invalid bincode blob
        db.conn.call(|conn| {
            conn.execute(
                "UPDATE traces SET leaf_nodes = X'DEADBEEF' WHERE id = 'lx'",
                [],
            )?;
            Ok(())
        }).await.unwrap();

        let entries = db.load_leaf_nodes().await;
        assert!(entries.is_empty());
    }

    // ─── find_traces_by_ids ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_find_traces_by_ids_subset() {
        let (db, _f) = test_db().await;
        for id in ["f1", "f2", "f3", "f4", "f5"] {
            db.flush_trace(id, None, &[], 1).await;
        }
        let results = db.find_traces_by_ids(&["f2".into(), "f4".into()]).await;
        assert_eq!(results.len(), 2);
        let mut ids: Vec<_> = results.iter().map(|r| r.trace_id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["f2", "f4"]);
    }

    #[tokio::test]
    async fn test_find_traces_by_ids_empty() {
        let (db, _f) = test_db().await;
        let results = db.find_traces_by_ids(&[]).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_find_traces_by_ids_missing() {
        let (db, _f) = test_db().await;
        let results = db.find_traces_by_ids(&["nonexistent".into()]).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_components_roundtrip() {
        let (db, _f) = test_db().await;
        db.flush_trace("cr", None, &["redis".into(), "auth".into()], 1).await;
        let rows = db.load_traces().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].components, vec!["redis", "auth"]);
    }

    #[tokio::test]
    async fn test_components_empty() {
        let (db, _f) = test_db().await;
        db.flush_trace("ce", None, &[], 1).await;
        let rows = db.load_traces().await;
        assert_eq!(rows.len(), 1);
        assert!(rows[0].components.is_empty());
    }

    // ─── prune ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_prune_removes_traces_and_thoughts() {
        let (db, _f) = test_db().await;
        db.flush_trace("p1", None, &[], 1).await;
        for i in 1..=3 {
            db.write_thought("p1", i, None, "{}", "{}", i as i64).await;
        }

        db.prune(&["p1".into()]).await;

        let (tc, trc): (i64, i64) = db.conn.call(|conn| {
            let tc = conn.query_row("SELECT COUNT(*) FROM thoughts WHERE trace_id='p1'", [], |r| r.get(0))?;
            let trc = conn.query_row("SELECT COUNT(*) FROM traces WHERE id='p1'", [], |r| r.get(0))?;
            Ok((tc, trc))
        }).await.unwrap();

        assert_eq!(tc, 0);
        assert_eq!(trc, 0);
    }

    #[tokio::test]
    async fn test_prune_only_targeted() {
        let (db, _f) = test_db().await;
        for id in ["q1", "q2", "q3"] {
            db.flush_trace(id, None, &[], 1).await;
            db.write_thought(id, 1, None, "{}", "{}", 1).await;
        }
        db.prune(&["q1".into()]).await;

        let remaining: i64 = db.conn.call(|conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM traces", [], |r| r.get(0))?)
        }).await.unwrap();
        assert_eq!(remaining, 2);

        let thoughts_remaining: i64 = db.conn.call(|conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM thoughts", [], |r| r.get(0))?)
        }).await.unwrap();
        assert_eq!(thoughts_remaining, 2);
    }

    #[tokio::test]
    async fn test_prune_empty_ids() {
        let (db, _f) = test_db().await;
        db.prune(&[]).await; // must not error
    }

    #[tokio::test]
    async fn test_prune_nonexistent_ids() {
        let (db, _f) = test_db().await;
        db.prune(&["nonexistent".into()]).await; // DELETE 0 rows, no error
    }
}
