-- The secret column of `sensor_configs` now also holds what a Kubernetes
-- check reads its cluster with (ADR-0183): the service account's token
-- and the cluster's CA, sealed together as one JSON object, the same way
-- (ADR-0125), never returned.
COMMENT ON COLUMN sensor_configs.secret IS
    'Sealed password of a website check (ADR-0150) or an application check (ADR-0183), or the token and CA of a Kubernetes check as one JSON object, opened only to send them to whoever runs the check.';
