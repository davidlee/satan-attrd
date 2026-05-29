-- Idle-decay scheduler state (design-contract §8 + §17.8).
--
-- T-attr-2 lands a daemon-side scheduler that applies a daily −0.01 tick to
-- the 4 negative-pole attributes (shame, doubt, brooding, metamorphosis).
-- `last_decay_at` is the per-row guard the scheduler reads ("fire if
-- (now - last_decay_at) ≥ 24h OR last_decay_at IS NULL") and bumps on a
-- successful tick.
--
-- The column lives on every row (not just the 4 decay-targets) because the
-- scheduler scans by name and bumping a single dedicated column is cheaper
-- than a side table. Unused rows retain whatever the backfill left.
--
-- Backfill semantics (§17.8 "Catch-up across migration / rebuild"):
--   SET last_decay_at = NOW() on existing rows so the first post-deploy tick
--   does NOT synthesise a "first time ever" multi-day catch-up against
--   pre-migration values. NULL is reserved for "decay never ran" — i.e.
--   rows created post-migration (default NULL) and rows reset by
--   rebuild_projection's §10.5 zero-step.

ALTER TABLE satan_attributes
  ADD COLUMN last_decay_at TIMESTAMPTZ NULL;

UPDATE satan_attributes SET last_decay_at = NOW();
