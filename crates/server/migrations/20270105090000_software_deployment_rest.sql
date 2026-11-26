-- Software deployment, the rest (ADR-0188): Linux packages in the
-- catalogue, the mirror a Linux entry installs from, and campaigns that
-- run again by themselves at each window.

-- apt and dnf entries name a package of the distributions and install
-- from the server's own sources, or from a mirror of the central server
-- that a campaign adds first. Only winget entries have a source.
ALTER TABLE software_catalog DROP CONSTRAINT software_catalog_manager_check;
ALTER TABLE software_catalog DROP CONSTRAINT software_catalog_source_check;
ALTER TABLE software_catalog
    ADD CONSTRAINT software_catalog_manager_check
        CHECK (manager IN ('winget', 'apt', 'dnf')),
    ADD CONSTRAINT software_catalog_source_check
        CHECK (CASE WHEN manager = 'winget' THEN char_length(source) BETWEEN 1 AND 64
                    ELSE source = '' END),
    ADD COLUMN mirror_id uuid REFERENCES mirrors (id) ON DELETE SET NULL,
    ADD CONSTRAINT software_catalog_mirror_check
        CHECK (mirror_id IS NULL OR manager <> 'winget');

-- A recurring campaign is run again by the runner at the first window
-- that opens after its last run completed, as "Run again" does
-- (ADR-0173). A failed or stopped run ends the recurrence until someone
-- runs it again.
ALTER TABLE campaigns ADD COLUMN recurring boolean NOT NULL DEFAULT false;
