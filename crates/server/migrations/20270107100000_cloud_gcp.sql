-- Google Cloud projects as cloud accounts (ADR-0183): `provider` may be
-- `gcp`, whose `settings` add `project` and whose sealed `secret` is the
-- service account's JSON key.
ALTER TABLE cloud_credentials DROP CONSTRAINT cloud_credentials_provider_check;
ALTER TABLE cloud_credentials ADD CONSTRAINT cloud_credentials_provider_check
    CHECK (provider IN ('aws', 'azure', 'gcp'));
