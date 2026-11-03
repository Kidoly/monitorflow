-- Change of certificate authority (ADR-0088): which CA signed each
-- certificate, and which CAs the peer trusts with it, as lowercase
-- hexadecimal SHA-256 of the DER CA certificates. The server fills
-- issuer_ca of older rows with its CA at start. trusted_cas is NULL for
-- peers too old to take a CA bundle: they trust the issuer only.
ALTER TABLE certificates
    ADD COLUMN issuer_ca text,
    ADD COLUMN trusted_cas text[];
