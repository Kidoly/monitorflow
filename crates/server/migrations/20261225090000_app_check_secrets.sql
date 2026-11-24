-- The password column of `sensor_configs` now also holds the password of
-- a ready-made application check (ADR-0183): PostgreSQL, MySQL and
-- MariaDB, Redis. Sealed the same way (ADR-0125), never returned.
COMMENT ON COLUMN sensor_configs.secret IS
    'Sealed password of a website check (ADR-0150) or an application check (ADR-0183), opened only to send it to whoever runs the check.';
