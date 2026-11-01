-- Called under the transaction-scoped allocation advisory lock.
-- The partial unique index on active hall_slot remains the final backstop.
SELECT candidate::integer AS hall_slot
FROM generate_series(
    0,
    (SELECT COUNT(*) FROM twz_games WHERE ended_at IS NULL)::integer
) AS candidate
WHERE NOT EXISTS (
    SELECT 1 FROM twz_games
    WHERE hall_slot = candidate AND ended_at IS NULL
)
ORDER BY candidate
LIMIT 1
