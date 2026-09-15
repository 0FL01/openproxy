//! Repository for durable API request-attempt logs.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct RequestDetailRow {
    pub id: String,
    pub timestamp: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub connection_id: Option<String>,
    pub status: Option<String>,
    pub api_key_id: Option<String>,
    pub api_key_name: Option<String>,
    pub correlation_id: Option<String>,
    pub data: Value,
}

pub struct NewRequestDetail<'a> {
    pub id: &'a str,
    pub timestamp: &'a str,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    pub connection_id: Option<&'a str>,
    pub status: &'a str,
    pub api_key_id: Option<&'a str>,
    pub api_key_name: Option<&'a str>,
    pub correlation_id: Option<&'a str>,
    pub data: &'a Value,
}

#[derive(Debug, Default)]
pub struct RequestDetailFilter<'a> {
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    pub connection_id: Option<&'a str>,
    pub status: Option<&'a str>,
    pub api_key_id: Option<&'a str>,
    pub correlation_id: Option<&'a str>,
    pub start_date: Option<&'a str>,
    pub end_date: Option<&'a str>,
}

pub fn insert(conn: &Connection, record: &NewRequestDetail<'_>) -> rusqlite::Result<()> {
    let data = serde_json::to_string(record.data).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO requestDetails(
            id, timestamp, provider, model, connectionId, status,\
            apiKeyId, apiKeyName, correlationId, data\
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            record.id,
            record.timestamp,
            record.provider,
            record.model,
            record.connection_id,
            record.status,
            record.api_key_id,
            record.api_key_name,
            record.correlation_id,
            data,
        ],
    )?;
    Ok(())
}

pub fn finish(conn: &Connection, id: &str, status: &str, data: &Value) -> rusqlite::Result<bool> {
    let data = serde_json::to_string(data).unwrap_or_else(|_| "{}".into());
    Ok(conn.execute(
        "UPDATE requestDetails SET status = ?2, data = ?3 WHERE id = ?1",
        params![id, status, data],
    )? > 0)
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<RequestDetailRow>> {
    conn.query_row(
        "SELECT id, timestamp, provider, model, connectionId, status,
                apiKeyId, apiKeyName, correlationId, data
         FROM requestDetails WHERE id = ?1",
        [id],
        row_from_sql,
    )
    .optional()
}

pub fn list(
    conn: &Connection,
    filter: &RequestDetailFilter<'_>,
    limit: usize,
    offset: usize,
) -> rusqlite::Result<Vec<RequestDetailRow>> {
    let mut statement = conn.prepare(
        "SELECT id, timestamp, provider, model, connectionId, status,
                apiKeyId, apiKeyName, correlationId, data
         FROM requestDetails
         WHERE (?1 IS NULL OR provider = ?1)
           AND (?2 IS NULL OR model = ?2)
           AND (?3 IS NULL OR connectionId = ?3)
           AND (?4 IS NULL OR status = ?4)
           AND (?5 IS NULL OR apiKeyId = ?5)
           AND (?6 IS NULL OR correlationId = ?6)
           AND (?7 IS NULL OR timestamp >= ?7)
           AND (?8 IS NULL OR timestamp <= ?8)
         ORDER BY timestamp DESC, id DESC
         LIMIT ?9 OFFSET ?10",
    )?;
    let rows = statement.query_map(
        params![
            filter.provider,
            filter.model,
            filter.connection_id,
            filter.status,
            filter.api_key_id,
            filter.correlation_id,
            filter.start_date,
            filter.end_date,
            limit as i64,
            offset as i64,
        ],
        row_from_sql,
    )?;
    rows.collect()
}

pub fn count(conn: &Connection, filter: &RequestDetailFilter<'_>) -> rusqlite::Result<usize> {
    conn.query_row(
        "SELECT COUNT(*) FROM requestDetails
         WHERE (?1 IS NULL OR provider = ?1)
           AND (?2 IS NULL OR model = ?2)
           AND (?3 IS NULL OR connectionId = ?3)
           AND (?4 IS NULL OR status = ?4)
           AND (?5 IS NULL OR apiKeyId = ?5)
           AND (?6 IS NULL OR correlationId = ?6)
           AND (?7 IS NULL OR timestamp >= ?7)
           AND (?8 IS NULL OR timestamp <= ?8)",
        params![
            filter.provider,
            filter.model,
            filter.connection_id,
            filter.status,
            filter.api_key_id,
            filter.correlation_id,
            filter.start_date,
            filter.end_date,
        ],
        |row| row.get::<_, i64>(0).map(|value| value as usize),
    )
}

pub fn mark_pending_interrupted(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE requestDetails SET status = 'interrupted' WHERE status = 'pending'",
        [],
    )
}

fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<RequestDetailRow> {
    let data: String = row.get(9)?;
    Ok(RequestDetailRow {
        id: row.get(0)?,
        timestamp: row.get(1)?,
        provider: row.get(2)?,
        model: row.get(3)?,
        connection_id: row.get(4)?,
        status: row.get(5)?,
        api_key_id: row.get(6)?,
        api_key_name: row.get(7)?,
        correlation_id: row.get(8)?,
        data: serde_json::from_str(&data).unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::SqliteDb;
    use serde_json::json;

    fn insert_record(db: &SqliteDb, id: &str, key_id: &str, status: &str) {
        db.with_conn(|conn| {
            insert(
                conn,
                &NewRequestDetail {
                    id,
                    timestamp: "2026-09-15T12:00:00Z",
                    provider: Some("openai"),
                    model: Some("gpt-5"),
                    connection_id: Some("connection-1"),
                    status,
                    api_key_id: Some(key_id),
                    api_key_name: Some("OpenCode"),
                    correlation_id: Some("request-1"),
                    data: &json!({"endpoint":"/v1/chat/completions"}),
                },
            )
        })
        .unwrap();
    }

    #[test]
    fn filters_and_paginates_by_api_key() {
        let db = SqliteDb::open_in_memory().unwrap();
        insert_record(&db, "attempt-1", "key-1", "success");
        insert_record(&db, "attempt-2", "key-2", "error");

        let filter = RequestDetailFilter {
            api_key_id: Some("key-1"),
            ..Default::default()
        };
        let rows = db.with_conn(|conn| list(conn, &filter, 20, 0)).unwrap();
        let total = db.with_conn(|conn| count(conn, &filter)).unwrap();

        assert_eq!(total, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "attempt-1");
        assert_eq!(rows[0].api_key_name.as_deref(), Some("OpenCode"));
        assert!(!rows[0].data.to_string().contains("raw-key"));
    }

    #[test]
    fn finishes_and_recovers_pending_attempts() {
        let db = SqliteDb::open_in_memory().unwrap();
        insert_record(&db, "attempt-1", "key-1", "pending");
        insert_record(&db, "attempt-2", "key-1", "pending");

        db.with_conn(|conn| {
            finish(conn, "attempt-1", "success", &json!({"durationMs": 42}))?;
            assert_eq!(mark_pending_interrupted(conn)?, 1);
            Ok(())
        })
        .unwrap();

        let first = db
            .with_conn(|conn| get(conn, "attempt-1"))
            .unwrap()
            .unwrap();
        let second = db
            .with_conn(|conn| get(conn, "attempt-2"))
            .unwrap()
            .unwrap();
        assert_eq!(first.status.as_deref(), Some("success"));
        assert_eq!(second.status.as_deref(), Some("interrupted"));
    }
}
