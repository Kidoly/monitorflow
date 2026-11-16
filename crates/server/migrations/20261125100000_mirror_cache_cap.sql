-- The storage cap of on-demand mirrors (ADR-0142), in bytes; NULL is the
-- default, 100 GiB or half the disk of mirror_dir.
ALTER TABLE server_settings
    ADD COLUMN mirror_cache_bytes bigint CHECK (mirror_cache_bytes >= 1073741824);
