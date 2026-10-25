-- Windows updates in package reports (ADR-0049): NULL, or false, for
-- Linux packages.
ALTER TABLE server_packages
    ADD COLUMN title           text CHECK (title <> '' AND octet_length(title) <= 1024),
    ADD COLUMN classification  text CHECK (classification <> '' AND octet_length(classification) <= 128),
    ADD COLUMN severity        text CHECK (severity <> '' AND octet_length(severity) <= 32),
    ADD COLUMN size_bytes      bigint CHECK (size_bytes > 0),
    -- Pending: installing it may need a restart. Installed: it waits for one.
    ADD COLUMN reboot_required boolean NOT NULL DEFAULT false;
