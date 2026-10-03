-- 0002_worker_payload.sql
-- Opaque, control-plane-owned bytes delivered with every claim.
-- DEFAULT '' covers rows created before this migration; new jobs always
-- write an explicit value.
ALTER TABLE jobs ADD COLUMN worker_payload text NOT NULL DEFAULT '';
