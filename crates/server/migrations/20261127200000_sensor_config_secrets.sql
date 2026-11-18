-- The password of a website check (ADR-0150), sealed like other secrets
-- (ADR-0125): `mfs1:<key id>:<sealed>`, never in `params`, never returned.
ALTER TABLE sensor_configs ADD COLUMN secret text;
COMMENT ON COLUMN sensor_configs.secret IS
    'Sealed password of a website check, opened only to send it to whoever runs the check; see ADR-0150.';
