-- Run in the expiry transaction. A concurrent fresh heartbeat causes the
-- predicate to be rechecked; an already-disconnected grace period is not reset.
UPDATE twz_games
SET disconnected_at = last_seen_at, connection_id = NULL
WHERE ended_at IS NULL
  AND disconnected_at IS NULL
  AND last_seen_at < NOW() - ($1::bigint * INTERVAL '1 second')
