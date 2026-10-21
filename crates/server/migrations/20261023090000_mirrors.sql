-- Package mirrors, their snapshots and the runs of the tools that build
-- them (ADR-0030). Mirrors are global: upstream content is the same for
-- every customer.
CREATE TABLE mirrors (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- A URL segment and part of aptly object names.
    name            text        NOT NULL UNIQUE CHECK (name ~ '^[a-z0-9][a-z0-9._-]{0,63}$'),
    family          text        NOT NULL CHECK (family IN ('apt', 'dnf')),
    -- No userinfo: credentials would end up in logs and aptly's database.
    upstream_url    text        NOT NULL CHECK (upstream_url ~ '^https?://[^/@?#]+(/[^@]*)?$'),
    distribution    text        NOT NULL DEFAULT '',
    components      text[]      NOT NULL DEFAULT '{}',
    architectures   text[]      NOT NULL DEFAULT '{}',
    -- NULL: synced only on request.
    sync_interval_s integer     CHECK (sync_interval_s >= 900),
    auto_snapshot   boolean     NOT NULL DEFAULT true,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (
        (family = 'apt' AND distribution <> '' AND cardinality(components) > 0)
        OR (family = 'dnf' AND distribution = '' AND cardinality(components) = 0)
    )
);
COMMENT ON TABLE mirrors IS 'Upstream repositories mirrored by the server with aptly or reposync. See ADR-0030.';
COMMENT ON COLUMN mirrors.distribution IS 'apt suite, like bookworm; empty for dnf.';
COMMENT ON COLUMN mirrors.architectures IS 'Empty: every architecture of the upstream.';

CREATE TABLE mirror_snapshots (
    id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    mirror_id    uuid        NOT NULL REFERENCES mirrors (id),
    -- A URL segment; `current` is the symlink to the newest published one.
    name         text        NOT NULL CHECK (name ~ '^[A-Za-z0-9_-][A-Za-z0-9._-]{0,63}$' AND name <> 'current'),
    state        text        NOT NULL DEFAULT 'creating' CHECK (state IN ('creating', 'published', 'failed')),
    -- Apparent size of the published tree; hardlinked files count in each.
    size_bytes   bigint      CHECK (size_bytes >= 0),
    error        text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    published_at timestamptz,
    UNIQUE (mirror_id, name),
    CHECK ((state = 'published') = (published_at IS NOT NULL))
);
COMMENT ON TABLE mirror_snapshots IS 'Frozen, signed states of a mirror, served under /<family>/<mirror>/<name>/.';

CREATE TABLE mirror_runs (
    id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    mirror_id    uuid        NOT NULL REFERENCES mirrors (id),
    kind         text        NOT NULL CHECK (kind IN ('sync', 'snapshot')),
    trigger      text        NOT NULL CHECK (trigger IN ('schedule', 'manual')),
    -- `user:<id>` or `cli`; NULL for the scheduler.
    requested_by text,
    state        text        NOT NULL DEFAULT 'running' CHECK (state IN ('running', 'succeeded', 'failed')),
    snapshot_id  uuid        REFERENCES mirror_snapshots (id),
    -- Commands and their output, the last 64 KiB.
    output       text        NOT NULL DEFAULT '',
    error        text,
    started_at   timestamptz NOT NULL DEFAULT now(),
    finished_at  timestamptz,
    CHECK ((state = 'running') = (finished_at IS NULL)),
    CHECK (state <> 'failed' OR error IS NOT NULL)
);
COMMENT ON TABLE mirror_runs IS 'Syncs and snapshots of mirrors with their tool output. See ADR-0030.';
CREATE INDEX mirror_runs_mirror_id_started_at_idx ON mirror_runs (mirror_id, started_at DESC);
-- Backs the advisory lock: one job per mirror at a time.
CREATE UNIQUE INDEX mirror_runs_one_running_idx ON mirror_runs (mirror_id) WHERE state = 'running';
CREATE INDEX mirror_snapshots_mirror_id_created_at_idx ON mirror_snapshots (mirror_id, created_at DESC);
