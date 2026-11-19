-- Report schedules (ADR-0155): an availability report sent by email each
-- month or week, for one customer or for everything, through an email
-- channel of the notifications (ADR-0060).

CREATE TABLE report_schedules (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- One customer's report; NULL for everything.
    customer_id     uuid        REFERENCES customers (id) ON DELETE CASCADE,
    frequency       text        NOT NULL DEFAULT 'monthly'
                                CHECK (frequency IN ('monthly', 'weekly')),
    -- IANA zone of the periods, the sending time (06:00) and the times
    -- written in the report.
    time_zone       text        NOT NULL CHECK (time_zone <> ''),
    language        text        NOT NULL DEFAULT 'en' CHECK (language IN ('en', 'fr')),
    pdf             boolean     NOT NULL DEFAULT true,
    csv             boolean     NOT NULL DEFAULT true,
    -- The email channel it is sent through; its recipients unless
    -- `recipients` names others.
    channel_id      uuid        NOT NULL REFERENCES notification_channels (id) ON DELETE CASCADE,
    recipients      text[]      NOT NULL DEFAULT '{}'
                                CHECK (cardinality(recipients) <= 20),
    enabled         boolean     NOT NULL DEFAULT true,
    -- The end of the last period sent: only the latest period is ever sent.
    last_period_end timestamptz,
    -- Held by the central server sending it, so two never both do.
    claimed_until   timestamptz,
    -- After a failure, not before this.
    retry_at        timestamptz,
    last_sent_at    timestamptz,
    last_error      text,
    last_error_at   timestamptz,
    created_by      uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (pdf OR csv)
);
COMMENT ON TABLE report_schedules IS
    'Availability reports sent by email each month or week. See ADR-0155.';
CREATE INDEX report_schedules_channel_idx ON report_schedules (channel_id);

-- The audit log says what was sent.
COMMENT ON COLUMN report_schedules.last_period_end IS
    'End of the latest period sent; report.sent in the audit log has each.';
