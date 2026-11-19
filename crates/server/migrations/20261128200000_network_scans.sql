-- Network discovery (ADR-0154): scans of a site's addresses, their runs,
-- what each run found, and the addresses dismissed per scan.

CREATE TABLE network_scans (
    id             uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    site_id        uuid        NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    name           text        NOT NULL
                   CHECK (char_length(name) BETWEEN 1 AND 128 AND name !~ '[[:cntrl:]]'),
    -- IP addresses, CIDR blocks and ranges as given; the server expands
    -- them to at most 1,024 addresses.
    targets        text[]      NOT NULL CHECK (cardinality(targets) BETWEEN 1 AND 64),
    ports          integer[]   NOT NULL DEFAULT '{}' CHECK (cardinality(ports) <= 32),
    probe          text        NOT NULL CHECK (probe IN ('central', 'relay')),
    probe_relay_id uuid,
    every          text        CHECK (every IN ('hour', 'day', 'week')),
    notify         boolean     NOT NULL DEFAULT false,
    -- The scan's own SNMP credentials, all of them as one sealed JSON
    -- object (ADR-0125); NULL when it has none.
    snmp_secret    text        CHECK (snmp_secret <> ''),
    created_by     uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    CHECK ((probe = 'relay') = (probe_relay_id IS NOT NULL)),
    FOREIGN KEY (probe_relay_id, site_id) REFERENCES relays (id, site_id) ON DELETE CASCADE
);
CREATE INDEX network_scans_site_idx ON network_scans (site_id);
COMMENT ON TABLE network_scans IS
    'Scans of a site''s addresses from a probe (ADR-0154); snmp_secret sealed (ADR-0125).';

CREATE TABLE network_scan_runs (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    scan_id       uuid        NOT NULL REFERENCES network_scans (id) ON DELETE CASCADE,
    state         text        NOT NULL DEFAULT 'queued'
                  CHECK (state IN ('queued', 'running', 'done', 'failed')),
    -- Who asked: a person, or the schedule.
    trigger       text        NOT NULL CHECK (trigger IN ('person', 'schedule')),
    requested_by  uuid        REFERENCES users (id) ON DELETE SET NULL,
    queued_at     timestamptz NOT NULL DEFAULT now(),
    started_at    timestamptz,
    progress_at   timestamptz,
    finished_at   timestamptz,
    addresses     integer     NOT NULL DEFAULT 0 CHECK (addresses >= 0),
    scanned       integer     NOT NULL DEFAULT 0 CHECK (scanned >= 0),
    answered      integer     NOT NULL DEFAULT 0 CHECK (answered >= 0),
    new_count     integer     NOT NULL DEFAULT 0 CHECK (new_count >= 0),
    -- Addresses that answered in the previous finished run and not in
    -- this one.
    gone          inet[]      NOT NULL DEFAULT '{}',
    -- Whether a finished run of the scan came before it, so "new" means
    -- something.
    compared      boolean     NOT NULL DEFAULT false,
    -- Answers of the probe that were not believed.
    dropped       integer     NOT NULL DEFAULT 0 CHECK (dropped >= 0),
    ping_error    text        CHECK (char_length(ping_error) <= 512),
    error         text        CHECK (char_length(error) <= 512),
    CHECK ((state = 'failed') = (error IS NOT NULL))
);
CREATE INDEX network_scan_runs_scan_idx ON network_scan_runs (scan_id, queued_at DESC);
-- One queued or running run per scan, whichever instance queues it.
CREATE UNIQUE INDEX network_scan_runs_one_active ON network_scan_runs (scan_id)
    WHERE state IN ('queued', 'running');
COMMENT ON TABLE network_scan_runs IS 'Runs of a network scan, the 10 latest kept (ADR-0154).';

CREATE TABLE network_scan_hosts (
    run_id          uuid             NOT NULL REFERENCES network_scan_runs (id) ON DELETE CASCADE,
    address         inet             NOT NULL,
    ping            boolean          NOT NULL,
    rtt_seconds     double precision CHECK (rtt_seconds >= 0),
    ports           integer[]        NOT NULL DEFAULT '{}',
    name            text             CHECK (char_length(name) <= 253),
    mac             text             CHECK (mac ~ '^[0-9a-f]{2}(:[0-9a-f]{2}){5}$'),
    -- Which credentials answered SNMP: the scan's own, or those of a
    -- device of the site (NULL once that device is removed).
    snmp_from_scan  boolean          NOT NULL DEFAULT false,
    snmp_device_id  uuid             REFERENCES servers (id) ON DELETE SET NULL,
    snmp            boolean          NOT NULL DEFAULT false,
    snmp_descr      text             CHECK (char_length(snmp_descr) <= 512),
    snmp_name       text             CHECK (char_length(snmp_name) <= 255),
    snmp_object_id  text             CHECK (snmp_object_id ~ '^[0-9]+(\.[0-9]+){0,127}$'),
    snmp_groups     text[]           NOT NULL DEFAULT '{}',
    template        text,
    is_new          boolean          NOT NULL DEFAULT false,
    PRIMARY KEY (run_id, address)
);
COMMENT ON TABLE network_scan_hosts IS 'What answered in a run of a network scan (ADR-0154).';

CREATE TABLE network_scan_dismissals (
    scan_id      uuid        NOT NULL REFERENCES network_scans (id) ON DELETE CASCADE,
    address      inet        NOT NULL,
    dismissed_by uuid        REFERENCES users (id) ON DELETE SET NULL,
    dismissed_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (scan_id, address)
);
COMMENT ON TABLE network_scan_dismissals IS
    'Addresses a person stopped a scan from proposing (ADR-0154).';

-- A scan that found something new or gone, for the scan's site.
ALTER TABLE notification_outbox DROP CONSTRAINT notification_outbox_event_check;
ALTER TABLE notification_outbox ADD CONSTRAINT notification_outbox_event_check
    CHECK (event IN ('incident.opened', 'incident.escalated', 'incident.resolved',
                     'approval.waiting', 'update.failed', 'backup.failed',
                     'update.available', 'ai_commands.changed', 'integration.disabled',
                     'network_scan.changed'));
