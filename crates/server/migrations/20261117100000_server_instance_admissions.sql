-- Central servers added under an Enterprise license (ADR-0100), by the
-- name of their host. An additional central server needs a place here to
-- start the first time; once it has one it starts whatever the license
-- says, so expiry stops adding central servers, never the ones running.
-- The first central server is never here: it always runs. Adding and
-- removing are in the audit log.

CREATE TABLE server_instance_admissions (
    name        text        PRIMARY KEY CHECK (name <> '' AND char_length(name) <= 253),
    admitted_at timestamptz NOT NULL DEFAULT now(),
    -- The license that had room; not a reference, as it may be replaced.
    license_id  uuid        NOT NULL
);
COMMENT ON TABLE server_instance_admissions IS
    'Additional central servers added under a license. See ADR-0100.';
