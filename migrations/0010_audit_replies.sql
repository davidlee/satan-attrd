-- Broker → daemon reject reply (design-contract §17.4 reject reply transport).
--
-- On §5.1 validator reject, the broker INSERTs (inbox_id, error_msg) here
-- and NOTIFY satan_audit_reply <inbox_id>.  Daemon LISTENs, SELECTs the row,
-- emits tracing::error!, and DELETEs.  Rejects-only — accept path is silent
-- (broker simply DELETEs the satan_audit_inbox row).

CREATE TABLE satan_audit_replies (
  inbox_id  INTEGER     PRIMARY KEY,
  ts        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  error_msg TEXT        NOT NULL
);
