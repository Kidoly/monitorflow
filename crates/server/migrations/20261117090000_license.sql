-- The Enterprise license key (ADR-0100): at most one, as the administrator
-- gave it. The server checks its signature each time it reads it, so a row
-- written by hand gains nothing. Installs and removals are in the audit
-- log. In the core schema, so a database is the same in every edition.

CREATE TABLE license (
    singleton    boolean     PRIMARY KEY DEFAULT true CHECK (singleton),
    text         text        NOT NULL CHECK (char_length(text) <= 16384),
    installed_at timestamptz NOT NULL DEFAULT now(),
    -- The username, kept if the user is deleted.
    installed_by text
);
COMMENT ON TABLE license IS
    'The Enterprise license key, checked on every read. See ADR-0100.';
