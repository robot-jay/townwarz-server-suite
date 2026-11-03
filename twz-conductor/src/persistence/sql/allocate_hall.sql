-- Called under the transaction-scoped allocation advisory lock.
-- $1 is the initial pool size; $2 is the authenticated account subject.
-- A pool of at least active-count + 2 always contains two free candidates,
-- so excluding the account's previous hall cannot exhaust the pool.
-- Reconnect-reserved games remain active (ended_at IS NULL) and are excluded.
WITH available AS (
    SELECT candidate
    FROM generate_series(
        0,
        GREATEST(
            $1::integer,
            (SELECT COUNT(*) FROM twz_games WHERE ended_at IS NULL)::integer + 2
        ) - 1
    ) AS candidates(candidate)
    WHERE NOT EXISTS (
        SELECT 1 FROM twz_games
        WHERE hall_slot = candidate AND ended_at IS NULL
    )
), previous_assignment AS (
    SELECT hall_slot FROM twz_games
    WHERE auth0_sub = $2
    ORDER BY participation_order DESC
    LIMIT 1
)
SELECT candidate::integer AS hall_slot
FROM available
WHERE candidate IS DISTINCT FROM (SELECT hall_slot FROM previous_assignment)
ORDER BY random()
LIMIT 1
