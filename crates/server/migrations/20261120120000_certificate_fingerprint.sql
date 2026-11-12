-- Renewal matches the presented certificate itself, not only its serial
-- (ADR-0121): lowercase hexadecimal SHA-256 of each issued certificate's
-- DER. NULL for rows from before this migration, which renew on their
-- serial except during an emergency change of certificate authority.
ALTER TABLE certificates
    ADD COLUMN cert_sha256 text CHECK (cert_sha256 ~ '^[0-9a-f]{64}$');
