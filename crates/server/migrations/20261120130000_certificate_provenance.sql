-- Where each renewed certificate came from (ADR-0028), so that revoking a
-- relay also revokes what was obtained through it.
--
-- renewed_from: serial of the certificate the peer presented to renew,
--   NULL for an enrollment.
-- relay_path: peer IDs of the relays a forwarded renewal went through,
--   from the one connected to the server down to the one the peer is
--   attached to. NULL for a direct renewal.
-- relay_serial: serial of the certificate the relay connected to the
--   server presented, NULL for a direct renewal.
ALTER TABLE certificates
    ADD COLUMN renewed_from bytea CHECK (octet_length(renewed_from) BETWEEN 1 AND 20),
    ADD COLUMN relay_path text[],
    ADD COLUMN relay_serial bytea CHECK (octet_length(relay_serial) BETWEEN 1 AND 20);

-- Revoking a relay walks these links (api::revoke).
CREATE INDEX certificates_renewed_from ON certificates (renewed_from)
    WHERE renewed_from IS NOT NULL;
CREATE INDEX certificates_relay_serial ON certificates (relay_serial)
    WHERE relay_serial IS NOT NULL;
CREATE INDEX certificates_relay_path ON certificates USING gin (relay_path)
    WHERE relay_path IS NOT NULL;

-- Earlier renewals: their audit rows hold the previous serial and the
-- relay the peer was attached to, not the whole path nor the relay's
-- serial. Read only; the audit log is never updated.
UPDATE certificates c
SET renewed_from = decode(a.details->>'previous_serial', 'hex'),
    relay_path = CASE WHEN a.details->>'via_relay' IS NULL THEN NULL
                      ELSE ARRAY[a.details->>'via_relay'] END
FROM audit_log a
WHERE a.action = 'peer.renew'
  AND a.details->>'serial' = encode(c.serial, 'hex')
  AND a.details->>'previous_serial' ~ '^([0-9a-f]{2}){1,20}$';
