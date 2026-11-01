-- Lock the complete record before checking/replacing its connection token.
-- Expiration and terminal updates cannot change it until the claim commits.
SELECT *
FROM twz_games
WHERE auth0_sub = $1
  AND ended_at IS NULL
  AND (
      disconnected_at IS NULL
      OR disconnected_at > NOW() - ($2::bigint * INTERVAL '1 second')
  )
ORDER BY started_at DESC
LIMIT 1
FOR UPDATE
