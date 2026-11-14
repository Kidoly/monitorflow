-- Check recommendations a person dismissed for one server (ADR-0134).
-- Keys are made by the server from its rules (`postgresql`,
-- `share:/mnt/backup`); the API stores only keys it recommends.
CREATE TABLE check_recommendation_dismissals (
    server_id    uuid        NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    key          text        NOT NULL CHECK (octet_length(key) BETWEEN 1 AND 300
                                             AND key !~ '[[:cntrl:]]'),
    dismissed_by uuid        REFERENCES users (id) ON DELETE SET NULL,
    dismissed_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (server_id, key)
);
COMMENT ON TABLE check_recommendation_dismissals IS 'Dismissed check recommendations per server; see ADR-0134.';
