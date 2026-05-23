-- Daemon → broker audit transcript queue (design-contract §17.4).
--
-- Daemon INSERTs one row per constructed attribute.delta_applied event,
-- then NOTIFY satan_audit_inbox <id>.  Broker LISTENs satan_audit_inbox,
-- claims the row, validates per §5.1, appends to transcript.jsonl, and
-- DELETEs.  On validator reject the broker writes a satan_audit_replies
-- row + NOTIFY satan_audit_reply <id> (§17.4 reject reply transport).
--
-- payload_json is the canonical attribute.delta_applied event the
-- transcript line will carry (§5).

CREATE TABLE satan_audit_inbox (
  id           SERIAL      PRIMARY KEY,
  payload_json JSONB       NOT NULL,
  enqueued_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  claimed_at   TIMESTAMPTZ
);

CREATE INDEX satan_audit_inbox_unclaimed_idx
  ON satan_audit_inbox (id)
  WHERE claimed_at IS NULL;
