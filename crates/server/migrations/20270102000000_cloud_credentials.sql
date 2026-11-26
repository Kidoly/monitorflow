-- How the central server reads a cloud account (ADR-0183): an AWS
-- account or an Azure subscription, one set per device. `settings` holds
-- what is not secret (access key ID or application ID, regions, tenant,
-- subscription); the secret access key or client secret is sealed in
-- `secret` (ADR-0125), never returned by the API.
CREATE TABLE cloud_credentials (
    server_id  uuid PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    provider   text  NOT NULL CHECK (provider IN ('aws', 'azure')),
    settings   jsonb NOT NULL CHECK (jsonb_typeof(settings) = 'object'),
    secret     text  NOT NULL CHECK (secret <> ''),
    updated_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE cloud_credentials IS
    'Read-only cloud credentials of a device; the secret sealed in secret (ADR-0125, ADR-0183).';
