-- Retention of mirror snapshots (ADR-0074). A removed snapshot keeps its
-- row, which runs and finished campaigns refer to: `pruning` from the
-- moment it is chosen until its files are gone, then `pruned`. Neither
-- has a `published_at`, so nothing can select them as published.
ALTER TABLE mirror_snapshots
    DROP CONSTRAINT mirror_snapshots_state_check,
    ADD CONSTRAINT mirror_snapshots_state_check
        CHECK (state IN ('creating', 'published', 'failed', 'pruning', 'pruned'));
