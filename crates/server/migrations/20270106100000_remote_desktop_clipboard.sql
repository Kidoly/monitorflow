-- The clipboard and file transfers of remote desktops (ADR-0192), each
-- switched by global administrators. A session keeps what the settings
-- said when it started; its proxy lets through only that.
ALTER TABLE remote_access_settings
    ADD COLUMN desktop_clipboard boolean NOT NULL DEFAULT true,
    ADD COLUMN desktop_files     boolean NOT NULL DEFAULT false;

ALTER TABLE remote_sessions
    ADD COLUMN clipboard     boolean NOT NULL DEFAULT false,
    ADD COLUMN file_transfer boolean NOT NULL DEFAULT false;
