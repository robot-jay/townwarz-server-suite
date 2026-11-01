-- RETURNING ensures only the caller that actually ends the game emits events.
-- A null token is the trusted subject-quit/terminal-end administrative path.
UPDATE twz_games
SET ended_at = NOW(), end_reason = $3, connection_id = NULL
WHERE player_id = $1
  AND ended_at IS NULL
  AND ($2::text IS NULL OR connection_id = $2)
RETURNING town_json
