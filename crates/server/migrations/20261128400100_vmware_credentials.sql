-- How probes log in to a vCenter or ESXi device (ADR-0156). One set per
-- device; the password sealed in `secret` (ADR-0125), never returned by
-- the API. `fingerprint` is the SHA-256 of a certificate a person
-- confirmed, for one the machine's roots do not trust.
CREATE TABLE vmware_credentials (
    server_id   uuid PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    port        integer NOT NULL DEFAULT 443 CHECK (port BETWEEN 1 AND 65535),
    username    text    NOT NULL CHECK (char_length(username) BETWEEN 1 AND 256 AND username !~ '[[:cntrl:]]'),
    fingerprint text    CHECK (fingerprint ~ '^([0-9A-F]{2}:){31}[0-9A-F]{2}$'),
    secret      text    NOT NULL CHECK (secret <> ''),
    updated_at  timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE vmware_credentials IS
    'vCenter or ESXi credentials of a device; the password sealed in secret (ADR-0125, ADR-0156).';
