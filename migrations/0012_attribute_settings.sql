-- Daemon-side settings table (design-contract §17.5 "Decay path" + §15 Q7).
--
-- T-attr-2d resolved §15 Q7 in favour of option A: the broker writes a row
-- here on every `dl-satan-attribute-updates-enabled` toggle, and the daemon's
-- DecayScheduler::tick SELECTs the row at tick start. The boolean threads
-- into MaintenanceInput.enabled → EventInsert.disabled and gates UPSERT +
-- last_decay_at bump per §17.5.
--
-- Schema is name-keyed JSONB so future settings (decay magnitudes, scheduler
-- intervals, etc.) reuse the same surface without further migrations.
-- updated_at is informational — the daemon does not consult it; the broker
-- write is the source of truth.
--
-- Seed: ('attribute_updates_enabled', 'true'::jsonb). Matches the broker
-- defcustom default (`t`); the broker first-load hook overwrites on next
-- emacs start if the operator has customised the value.

CREATE TABLE satan_attribute_settings (
  name       TEXT        PRIMARY KEY,
  value      JSONB       NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO satan_attribute_settings (name, value)
VALUES ('attribute_updates_enabled', 'true'::jsonb);
