-- History lookup shares the allocation lock with the new claim insert.
SELECT COALESCE(MAX((town_json->>'created_at_ms')::bigint), 0) AS created_at_ms
FROM twz_games
WHERE hall_slot = $1
