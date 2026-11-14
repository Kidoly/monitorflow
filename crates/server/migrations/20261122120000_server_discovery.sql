-- What each server's agent last saw running on its machine (ADR-0134):
-- running services, listening ports and data file systems, bounded by
-- the server before they are written. One row per server, replaced by
-- every report; recommendations are computed from it when asked.
CREATE TABLE server_discovery (
    server_id   uuid        PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    report      jsonb       NOT NULL CHECK (jsonb_typeof(report) = 'object'
                                            AND octet_length(report::text) <= 262144),
    reported_at timestamptz NOT NULL,
    -- Entries of the report that were malformed or over the caps.
    dropped     integer     NOT NULL DEFAULT 0 CHECK (dropped >= 0)
);
COMMENT ON TABLE server_discovery IS 'Latest discovery report per server, bounded; see ADR-0134.';
