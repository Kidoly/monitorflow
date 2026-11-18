-- Business services (ADR-0155): a named group of servers, devices or
-- checks of one customer with one availability, by a rule (all of them up,
-- or at least N) and an optional SLA target.

CREATE TABLE business_services (
    id          uuid             PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id uuid             NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
    name        text             NOT NULL CHECK (name <> '' AND char_length(name) <= 80),
    description text             NOT NULL DEFAULT '' CHECK (char_length(description) <= 500),
    -- 'all': down while any member is; 'at_least': down while fewer than
    -- `at_least` members are up.
    rule        text             NOT NULL DEFAULT 'all' CHECK (rule IN ('all', 'at_least')),
    at_least    integer          CHECK (at_least BETWEEN 1 AND 1000),
    -- A percentage, like 99.9; NULL without a target.
    target      double precision CHECK (target >= 50 AND target <= 100),
    created_by  uuid             REFERENCES users (id) ON DELETE SET NULL,
    created_at  timestamptz      NOT NULL DEFAULT now(),
    updated_at  timestamptz      NOT NULL DEFAULT now(),
    CHECK ((rule = 'at_least') = (at_least IS NOT NULL)),
    CONSTRAINT business_services_name_key UNIQUE (customer_id, name)
);
COMMENT ON TABLE business_services IS
    'Named groups of servers, devices and checks with one availability. See ADR-0155.';

-- A member: a whole server or device, or one check of it, by the identity
-- silences use (ADR-0151). A server of another customer is refused by
-- the API, and reports only read members within the report's sites.
CREATE TABLE business_service_members (
    service_id     uuid    NOT NULL REFERENCES business_services (id) ON DELETE CASCADE,
    server_id      uuid    NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    check_name     text    CHECK (check_name ~ '^[a-z][a-z0-9_]{0,63}$'),
    check_instance text    NOT NULL DEFAULT ''
                           CHECK (octet_length(check_instance) <= 256
                                  AND check_instance !~ '[[:cntrl:]]'),
    position       integer NOT NULL DEFAULT 0,
    CHECK (check_name IS NOT NULL OR check_instance = ''),
    CONSTRAINT business_service_members_key
        UNIQUE NULLS NOT DISTINCT (service_id, server_id, check_name, check_instance)
);
CREATE INDEX business_service_members_server_idx ON business_service_members (server_id);

-- Reports read the incidents of a period: their start, and the end of
-- those already resolved.
CREATE INDEX incidents_relay_opened_idx ON incidents (relay_id, opened_at)
    WHERE relay_id IS NOT NULL;
