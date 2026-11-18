-- How probes authenticate to a device over SNMP (ADR-0149). One set per
-- device; the community and passwords sealed together in `secret`
-- (ADR-0125), as a JSON object, never returned by the API.
CREATE TABLE snmp_credentials (
    server_id     uuid PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    version       text NOT NULL CHECK (version IN ('v2c', 'v3')),
    port          integer NOT NULL DEFAULT 161 CHECK (port BETWEEN 1 AND 65535),
    username      text CHECK (char_length(username) BETWEEN 1 AND 256 AND username !~ '[[:cntrl:]]'),
    auth_protocol text CHECK (auth_protocol IN ('md5', 'sha', 'sha256', 'sha384', 'sha512')),
    priv_protocol text CHECK (priv_protocol IN ('aes128', 'aes256', 'aes256c')),
    secret        text NOT NULL CHECK (secret <> ''),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CHECK ((version = 'v3') = (username IS NOT NULL)),
    CHECK (version = 'v3' OR (auth_protocol IS NULL AND priv_protocol IS NULL)),
    CHECK (priv_protocol IS NULL OR auth_protocol IS NOT NULL)
);
COMMENT ON TABLE snmp_credentials IS
    'SNMP credentials of a device; secrets sealed in secret (ADR-0125, ADR-0149).';
