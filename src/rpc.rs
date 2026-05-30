//! Daemon → broker RPC over the `satan_audit_inbox` queue
//! (design-contract §17.4).
//!
//! The daemon enqueues a constructed `attribute.delta_applied` payload onto
//! the inbox + `pg_notify`s the broker. The broker LISTENs `satan_audit_inbox`,
//! claims the row, validates per §5.1, appends to `transcript.jsonl`, and
//! DELETEs.
//!
//! Accept path is silent (no reply row). On reject the broker writes
//! `satan_audit_replies` + `pg_notify satan_audit_reply`; the run loop's
//! reply `LISTENer` (in `main.rs`) handles those.

use serde_json::Value;
use sqlx::PgPool;
use sqlx::types::Json;

use crate::error::Result;

/// Channel the broker LISTENs on for new daemon → broker audit events.
pub const AUDIT_INBOX_CHANNEL: &str = "satan_audit_inbox";

/// Channel the daemon LISTENs on for broker reject replies.
pub const AUDIT_REPLY_CHANNEL: &str = "satan_audit_reply";

/// Channel the daemon LISTENs on for new broker → daemon outcome events.
pub const OUTCOME_INBOX_CHANNEL: &str = "satan_outcome_inbox";

/// Payload `schema_version` (major.minor) accepted by the v1 daemon.
/// Daemon rejects payloads whose **major** does not match (§17.3).
pub const PAYLOAD_SCHEMA_MAJOR: u32 = 1;

/// Wire-shape `schema_version` string the daemon stamps on outbound payloads.
pub const PAYLOAD_SCHEMA_VERSION: &str = "1.0";

/// Enqueue one `attribute.delta_applied` payload onto `satan_audit_inbox`
/// and fire `pg_notify satan_audit_inbox <id>`. Returns the new row id.
///
/// The payload is the canonical event body as defined in §5; the broker's
/// audit validator runs against it verbatim after claiming the row.
///
/// # Errors
///
/// Returns a Sqlx error on database failure.
pub async fn enqueue_audit_event(pool: &PgPool, payload: &Value) -> Result<i32> {
    let (id,): (i32,) = sqlx::query_as(
        "INSERT INTO satan_audit_inbox (payload_json)
         VALUES ($1)
         RETURNING id",
    )
    .bind(Json(payload))
    .fetch_one(pool)
    .await?;

    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(AUDIT_INBOX_CHANNEL)
        .bind(id.to_string())
        .execute(pool)
        .await?;

    Ok(id)
}

/// Stamp the daemon's outbound `schema_version` onto a payload. Mutates the
/// object in place; returns it for fluent use.
#[must_use]
pub fn with_schema_version(mut payload: Value) -> Value {
    if let Value::Object(ref mut map) = payload {
        map.insert(
            "schema_version".to_string(),
            Value::String(PAYLOAD_SCHEMA_VERSION.to_string()),
        );
    }
    payload
}

/// Parse + check a payload's `schema_version` against the daemon's compiled
/// major. Returns `Ok(())` on accept or `Err(reason)` on reject.
///
/// Missing field, non-string field, malformed `MAJOR.MINOR`, or major
/// mismatch all reject; minor differences accept (forward compat).
///
/// # Errors
///
/// Returns `Err(reason)` if: `schema_version` is missing, is not a string,
/// is malformed, or its major component does not match `PAYLOAD_SCHEMA_MAJOR`.
pub fn check_schema_major(payload: &Value) -> std::result::Result<(), String> {
    let v = payload
        .get("schema_version")
        .ok_or_else(|| "missing schema_version".to_string())?;
    let s = v
        .as_str()
        .ok_or_else(|| format!("schema_version must be string, got {v}"))?;
    let major_str = s
        .split('.')
        .next()
        .ok_or_else(|| format!("malformed schema_version: {s:?}"))?;
    let major: u32 = major_str
        .parse()
        .map_err(|e| format!("malformed schema_version major: {s:?}: {e}"))?;
    if major != PAYLOAD_SCHEMA_MAJOR {
        return Err(format!(
            "schema_version major {major} does not match daemon major {PAYLOAD_SCHEMA_MAJOR}"
        ));
    }
    Ok(())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test fixtures: unwrap_err on Err variant, index into known JSON keys"
)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schema_version_accepts_matching_major() {
        assert!(check_schema_major(&json!({ "schema_version": "1.0" })).is_ok());
        assert!(check_schema_major(&json!({ "schema_version": "1.7" })).is_ok());
    }

    #[test]
    fn schema_version_rejects_missing() {
        let err = check_schema_major(&json!({})).unwrap_err();
        assert!(err.contains("missing"));
    }

    #[test]
    fn schema_version_rejects_major_mismatch() {
        let err = check_schema_major(&json!({ "schema_version": "2.0" })).unwrap_err();
        assert!(err.contains("major 2"));
    }

    #[test]
    fn schema_version_rejects_non_string() {
        let err = check_schema_major(&json!({ "schema_version": 1.0 })).unwrap_err();
        assert!(err.contains("must be string"));
    }

    #[test]
    fn schema_version_rejects_malformed() {
        let err = check_schema_major(&json!({ "schema_version": "abc" })).unwrap_err();
        assert!(err.contains("malformed"));
    }

    #[test]
    fn with_schema_version_inserts_field() {
        let v = with_schema_version(json!({ "foo": "bar" }));
        assert_eq!(v["schema_version"], json!("1.0"));
        assert_eq!(v["foo"], json!("bar"));
    }
}
