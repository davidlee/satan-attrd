-- Broker → daemon outcome queue (design-contract §17.3).
--
-- Broker INSERTs one row per intervention.outcome_classified /
-- intervention.outcome_revised event, then NOTIFY satan_outcome_inbox <id>.
-- Daemon LISTENs satan_outcome_inbox, claims the row, dispatches per §6 + §7,
-- and DELETEs after writing the satan_attribute_events row(s).
--
-- payload_json shape pinned in §17.3 (v1.0).  Validation is daemon-side; the
-- broker only ensures schema_version + required keys are present at enqueue
-- time so the daemon can reject malformed rows without re-reading the
-- transcript.

CREATE TABLE satan_outcome_inbox (
  id           SERIAL      PRIMARY KEY,
  payload_json JSONB       NOT NULL,
  enqueued_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  claimed_at   TIMESTAMPTZ
);

CREATE INDEX satan_outcome_inbox_unclaimed_idx
  ON satan_outcome_inbox (id)
  WHERE claimed_at IS NULL;
