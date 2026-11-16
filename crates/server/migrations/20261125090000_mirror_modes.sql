-- On-demand mirrors (ADR-0142): a mirror downloads every package (`full`,
-- what every existing mirror does) or only its indexes, its packages
-- being fetched on first request (`on_demand`). Each snapshot records the
-- mode it was built in, so both kinds coexist after a switch and each is
-- served and removed by its own rules.
ALTER TABLE mirrors
    ADD COLUMN mode text NOT NULL DEFAULT 'full' CHECK (mode IN ('full', 'on_demand'));
ALTER TABLE mirror_snapshots
    ADD COLUMN mode text NOT NULL DEFAULT 'full' CHECK (mode IN ('full', 'on_demand'));
COMMENT ON COLUMN mirrors.mode IS 'full or on_demand, for the next sync; see ADR-0142.';
COMMENT ON COLUMN mirror_snapshots.mode IS 'The mode the snapshot was built in; see ADR-0142.';
