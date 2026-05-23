-- Attribute layer storage (design-contract §4 + §6.2.1).
--
-- Two tables in the SATAN database:
--   satan_attributes        — current state projection (one row per (scope, name))
--   satan_attribute_events  — append-only event log; projection is derivable
--                              via ORDER BY ts, run_id, seq (§10).
--
-- Eight attributes are seeded at scope='global' value=0.0 so the layer is
-- queryable from the moment the daemon is enabled (no null semantics).

-- ============================================================
-- 4.1 Projection
-- ============================================================

CREATE TABLE satan_attributes (
  scope            TEXT NOT NULL,
  name             TEXT NOT NULL,
  value            DOUBLE PRECISION NOT NULL CHECK (value >= 0 AND value <= 1),
  updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  evidence_json    JSONB NOT NULL DEFAULT '{}'::jsonb,
  PRIMARY KEY (scope, name)
);

-- ============================================================
-- 4.2 Event log (append-only)
-- ============================================================

CREATE TABLE satan_attribute_events (
  id               TEXT PRIMARY KEY,
  ts               TIMESTAMPTZ NOT NULL,
  run_id           TEXT NOT NULL,
  seq              INTEGER NOT NULL,
  scope            TEXT NOT NULL,
  name             TEXT NOT NULL,
  old_value        DOUBLE PRECISION NOT NULL,
  new_value        DOUBLE PRECISION NOT NULL,
  delta            DOUBLE PRECISION NOT NULL,
  source           TEXT NOT NULL,
  reason           TEXT NOT NULL,
  evidence_json    JSONB NOT NULL DEFAULT '{}'::jsonb,
  caps_applied     JSONB NOT NULL DEFAULT '[]'::jsonb,
  disabled         BOOLEAN NOT NULL DEFAULT false,
  UNIQUE (run_id, seq)
);

CREATE INDEX satan_attribute_events_run_idx
  ON satan_attribute_events (run_id, seq);

CREATE INDEX satan_attribute_events_name_idx
  ON satan_attribute_events (scope, name, ts DESC);

CREATE INDEX satan_attribute_events_replay_idx
  ON satan_attribute_events (ts, run_id, seq);

-- §6.2.1 — revision algorithm walks back through prior events for the same
-- intervention via evidence_json->>'intervention_id'.  Expression index keeps
-- that lookup cheap.
CREATE INDEX satan_attribute_events_iv_idx
  ON satan_attribute_events ((evidence_json->>'intervention_id'));

-- ============================================================
-- Seed 8 attributes at value=0.0 / scope='global'
-- ============================================================

INSERT INTO satan_attributes (scope, name, value, evidence_json) VALUES
  ('global', 'curiosity',     0.0, '{}'::jsonb),
  ('global', 'hunger',        0.0, '{}'::jsonb),
  ('global', 'suspicion',     0.0, '{}'::jsonb),
  ('global', 'doubt',         0.0, '{}'::jsonb),
  ('global', 'friction',      0.0, '{}'::jsonb),
  ('global', 'shame',         0.0, '{}'::jsonb),
  ('global', 'brooding',      0.0, '{}'::jsonb),
  ('global', 'metamorphosis', 0.0, '{}'::jsonb);
