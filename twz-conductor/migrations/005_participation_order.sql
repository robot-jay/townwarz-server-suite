-- Wall clocks can move backwards (including after VM clock synchronization).
-- Participation order must not depend on started_at or random player UUIDs.
CREATE SEQUENCE IF NOT EXISTS twz_participation_order_seq;
ALTER TABLE twz_games ADD COLUMN IF NOT EXISTS participation_order BIGINT;
WITH history AS (
    SELECT player_id,
           ROW_NUMBER() OVER (ORDER BY started_at, player_id)
             + COALESCE((SELECT MAX(participation_order) FROM twz_games), 0) AS ordinal
    FROM twz_games WHERE participation_order IS NULL
)
UPDATE twz_games SET participation_order = history.ordinal
FROM history WHERE twz_games.player_id = history.player_id;
SELECT setval('twz_participation_order_seq',
    GREATEST(COALESCE((SELECT MAX(participation_order) FROM twz_games), 0),
             (SELECT last_value FROM twz_participation_order_seq)), true);
ALTER TABLE twz_games ALTER COLUMN participation_order
    SET DEFAULT nextval('twz_participation_order_seq');
ALTER TABLE twz_games ALTER COLUMN participation_order SET NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS twz_games_participation_order ON twz_games(participation_order);
