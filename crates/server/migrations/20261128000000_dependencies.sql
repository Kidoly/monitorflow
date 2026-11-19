-- Dependencies (ADR-0152): a server, a device or one of its checks is
-- behind a parent, which speaks for it while it is down. Add-only.

-- A child is a server or device (`server_id`), or one of its checks when
-- `check_name` is set. A parent is a server or device, or one of its checks
-- when `parent_check_name` is set, or a relay. An instance of '' is every
-- instance of the check, as silences name checks (ADR-0151).
CREATE TABLE dependencies (
    id                    uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    server_id             uuid        NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    check_name            text        CHECK (check_name ~ '^[a-z][a-z0-9_]{0,63}$'),
    check_instance        text        NOT NULL DEFAULT ''
                                      CHECK (octet_length(check_instance) <= 256
                                             AND check_instance !~ '[[:cntrl:]]'),
    parent_server_id      uuid        REFERENCES servers (id) ON DELETE CASCADE,
    parent_relay_id       uuid        REFERENCES relays (id) ON DELETE CASCADE,
    parent_check_name     text        CHECK (parent_check_name ~ '^[a-z][a-z0-9_]{0,63}$'),
    parent_check_instance text        NOT NULL DEFAULT ''
                                      CHECK (octet_length(parent_check_instance) <= 256
                                             AND parent_check_instance !~ '[[:cntrl:]]'),
    created_by            uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at            timestamptz NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(parent_server_id, parent_relay_id) = 1),
    CHECK (parent_check_name IS NULL OR parent_server_id IS NOT NULL),
    CHECK (check_name IS NOT NULL OR check_instance = ''),
    CHECK (parent_check_name IS NOT NULL OR parent_check_instance = ''),
    -- Within one server, only a check on another of its checks.
    CONSTRAINT dependencies_not_on_itself CHECK (
        parent_server_id IS DISTINCT FROM server_id
        OR (check_name IS NOT NULL AND parent_check_name IS NOT NULL
            AND (check_name, check_instance) <> (parent_check_name, parent_check_instance))),
    CONSTRAINT dependencies_key UNIQUE NULLS NOT DISTINCT
        (server_id, check_name, check_instance, parent_server_id, parent_relay_id,
         parent_check_name, parent_check_instance)
);
COMMENT ON TABLE dependencies IS
    'What is behind what: a child opens no incident while a parent is down; see ADR-0152.';
CREATE INDEX dependencies_parent_server_idx ON dependencies (parent_server_id)
    WHERE parent_server_id IS NOT NULL;
CREATE INDEX dependencies_parent_relay_idx ON dependencies (parent_relay_id)
    WHERE parent_relay_id IS NOT NULL;

-- A dependency never crosses customers: both ends' sites belong to one.
CREATE FUNCTION dependencies_one_customer() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    child  uuid;
    parent uuid;
BEGIN
    SELECT si.customer_id INTO child
    FROM servers s JOIN sites si ON si.id = s.site_id WHERE s.id = NEW.server_id;
    IF NEW.parent_server_id IS NOT NULL THEN
        SELECT si.customer_id INTO parent
        FROM servers s JOIN sites si ON si.id = s.site_id WHERE s.id = NEW.parent_server_id;
    ELSE
        SELECT si.customer_id INTO parent
        FROM relays r JOIN sites si ON si.id = r.site_id WHERE r.id = NEW.parent_relay_id;
    END IF;
    IF child IS DISTINCT FROM parent THEN
        RAISE EXCEPTION 'a dependency never crosses customers'
            USING ERRCODE = 'check_violation', CONSTRAINT = 'dependencies_one_customer';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER dependencies_one_customer
    BEFORE INSERT OR UPDATE ON dependencies
    FOR EACH ROW EXECUTE FUNCTION dependencies_one_customer();

-- The template a device was added with (ADR-0149), to suggest switches,
-- routers and firewalls as parents. NULL for devices added without one.
ALTER TABLE servers
    ADD COLUMN device_template text
        CHECK (device_template IN ('switch', 'router', 'firewall', 'printer', 'ups', 'nas', 'generic'));

-- An incident whose child went behind a parent that was down by the time
-- the dispatcher would have told it: never told, listed under the
-- parent's incident (ADR-0152).
ALTER TABLE incidents
    ADD COLUMN hidden_by uuid REFERENCES incidents (id) ON DELETE SET NULL;
CREATE INDEX incidents_hidden_by_idx ON incidents (hidden_by) WHERE hidden_by IS NOT NULL;

-- An import may create dependencies, which its undo removes. 'service'
-- is the availability reports' (20261128300002), whichever applies last.
ALTER TABLE import_created DROP CONSTRAINT import_created_object_check;
ALTER TABLE import_created ADD CONSTRAINT import_created_object_check
    CHECK (object IN ('server', 'check', 'threshold', 'silence', 'window', 'channel',
                      'dependency', 'service'));
