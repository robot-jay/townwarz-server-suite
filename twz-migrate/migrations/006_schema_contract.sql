-- Additive release contract. Future additive migrations increment schema_version
-- while retaining minimum_reader_version; breaking changes must raise the latter.
CREATE TABLE IF NOT EXISTS twz_schema_metadata (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    schema_version INTEGER NOT NULL CHECK (schema_version > 0),
    minimum_reader_version INTEGER NOT NULL CHECK (minimum_reader_version > 0)
);
INSERT INTO twz_schema_metadata (id, schema_version, minimum_reader_version)
VALUES (1, 6, 1);
