ALTER TABLE twz_games ADD COLUMN IF NOT EXISTS connection_id TEXT;
ALTER TABLE twz_games ADD COLUMN IF NOT EXISTS disconnected_at TIMESTAMPTZ;
ALTER TABLE twz_games ADD COLUMN IF NOT EXISTS checkpoint_json JSONB;
ALTER TABLE twz_games ADD COLUMN IF NOT EXISTS end_reason TEXT;
CREATE INDEX IF NOT EXISTS twz_games_hall_history ON twz_games(hall_slot);
