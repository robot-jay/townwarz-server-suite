-- A row lock on the owning game serializes economic commands and lifecycle
-- transitions. NULL preserves legacy checkpoints until economy is first used.
ALTER TABLE twz_games ADD COLUMN IF NOT EXISTS economy_json JSONB;
