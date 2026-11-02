-- Do not split token validation and checkpoint writing into separate queries.
UPDATE twz_games
SET last_seen_at = NOW(),
    checkpoint_json = COALESCE($3::jsonb, checkpoint_json)
WHERE player_id = $1
  AND connection_id = $2
  AND ended_at IS NULL
