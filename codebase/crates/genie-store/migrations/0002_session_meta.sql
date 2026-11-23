-- The daemon's session registry (titles, model, workspace, flags) persisted per row, so
-- the session list survives a restart. NULL on rows written before this migration.
ALTER TABLE session ADD COLUMN meta TEXT CHECK (meta IS NULL OR json_valid(meta));
