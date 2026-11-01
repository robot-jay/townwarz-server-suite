-- A late disconnect from an old socket must not clear a resumed connection.
UPDATE twz_games
SET disconnected_at = NOW(),
    connection_id = NULL,
    last_seen_at = NOW(),
    checkpoint_json = COALESCE($3::jsonb, checkpoint_json)
WHERE player_id = $1
  AND connection_id = $2
  AND ended_at IS NULL
