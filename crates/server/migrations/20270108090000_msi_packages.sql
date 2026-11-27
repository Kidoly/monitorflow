-- MSI packages in the software catalogue (ADR-0188).

-- Uploaded MSI files, stored once by SHA-256 under <mirror_dir>/software/
-- and served by the mirror listeners. A file nothing names any more
-- (no catalogue entry, no campaign) gets `unreferenced_since`, and goes
-- 8 days later, past a command's longest time to live.
CREATE TABLE software_files (
    sha256             text        PRIMARY KEY CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    size               bigint      NOT NULL CHECK (size BETWEEN 1 AND 2147483648),
    -- Read from the file's Property table; NULL when it has none.
    product_code       text,
    product_name       text        NOT NULL DEFAULT '',
    product_version    text        NOT NULL DEFAULT '',
    manufacturer       text        NOT NULL DEFAULT '',
    -- As the uploader named it, for people only.
    file_name          text        NOT NULL DEFAULT '' CHECK (char_length(file_name) <= 255),
    -- Who uploaded it: they may add it to the catalogue.
    uploaded_by        uuid[]      NOT NULL DEFAULT '{}',
    uploaded_at        timestamptz NOT NULL DEFAULT now(),
    unreferenced_since timestamptz DEFAULT now()
);
COMMENT ON TABLE software_files IS
    'MSI files of the software catalogue, by SHA-256; see ADR-0188.';

-- An msi entry names its file and the public properties passed to
-- msiexec ([{"name", "value"}]); its package_id is the ProductCode.
ALTER TABLE software_catalog DROP CONSTRAINT software_catalog_manager_check;
ALTER TABLE software_catalog DROP CONSTRAINT software_catalog_mirror_check;
ALTER TABLE software_catalog
    ADD CONSTRAINT software_catalog_manager_check
        CHECK (manager IN ('winget', 'apt', 'dnf', 'msi')),
    ADD CONSTRAINT software_catalog_mirror_check
        CHECK (mirror_id IS NULL OR manager IN ('apt', 'dnf')),
    ADD COLUMN file_sha256 text REFERENCES software_files (sha256),
    ADD COLUMN properties  jsonb NOT NULL DEFAULT '[]'
        CHECK (jsonb_typeof(properties) = 'array'),
    ADD CONSTRAINT software_catalog_file_check
        CHECK ((manager = 'msi') = (file_sha256 IS NOT NULL));
CREATE INDEX software_catalog_file ON software_catalog (file_sha256)
    WHERE file_sha256 IS NOT NULL;
