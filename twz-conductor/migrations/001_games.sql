CREATE TABLE IF NOT EXISTS twz_users (
    auth0_sub TEXT PRIMARY KEY,
    display_name TEXT,
    email TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS twz_games (
    player_id TEXT PRIMARY KEY,
    auth0_sub TEXT NOT NULL REFERENCES twz_users(auth0_sub),
    role TEXT NOT NULL DEFAULT 'mayor' CHECK (role = 'mayor'),
    town_id TEXT NOT NULL UNIQUE,
    town_name TEXT NOT NULL CHECK (char_length(town_name) BETWEEN 2 AND 32),
    hall_slot INTEGER NOT NULL CHECK (hall_slot >= 0),
    town_json JSONB NOT NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ended_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX IF NOT EXISTS twz_games_active_hall
    ON twz_games(hall_slot) WHERE ended_at IS NULL;
CREATE INDEX IF NOT EXISTS twz_games_user ON twz_games(auth0_sub);
