-- When the central server was not running (ADR-0173). Every instance
-- moves `central_server_heartbeat` forward on each tick; an instance that
-- finds it older than an instance's death delay writes the stretch in
-- between to `central_server_gaps`. Nothing was watched then, so
-- availability leaves that time out of the count instead of keeping the
-- incidents of before as they were.
CREATE TABLE central_server_heartbeat (
    id boolean     PRIMARY KEY DEFAULT true CHECK (id),
    at timestamptz NOT NULL
);
COMMENT ON TABLE central_server_heartbeat IS
    'The last heartbeat of any central server instance, one row. See ADR-0173.';

CREATE TABLE central_server_gaps (
    starts_at timestamptz NOT NULL,
    ends_at   timestamptz NOT NULL CHECK (ends_at > starts_at)
);
CREATE INDEX central_server_gaps_ends_at_idx ON central_server_gaps (ends_at);
COMMENT ON TABLE central_server_gaps IS
    'Stretches of time no central server instance was running. See ADR-0173.';
