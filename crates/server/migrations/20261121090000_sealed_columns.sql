-- Columns whose values are all sealed with the server's secrets key
-- (ADR-0125). The key is outside the database, so the server seals the
-- values at startup, not this migration: for each column it seals that is
-- not listed here, it seals every value and adds the column, in one
-- transaction. A column is never sealed twice.
CREATE TABLE sealed_columns (
    name      text        PRIMARY KEY CHECK (name <> ''),
    sealed_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE sealed_columns IS
    'Columns holding values sealed with the secrets key in the data directory (mfs1:<key id>:<sealed>). See ADR-0125.';
