-- Atomically expire and return the affected records, rather than selecting
-- IDs and later unconditionally ending games that may have resumed meanwhile.
UPDATE twz_games
SET ended_at = NOW(), end_reason = 'timeout', connection_id = NULL
WHERE ended_at IS NULL
  AND disconnected_at < NOW() - ($1::bigint * INTERVAL '1 second')
RETURNING town_json
