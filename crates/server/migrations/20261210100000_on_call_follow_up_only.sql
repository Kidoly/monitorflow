-- Schedules for follow-ups only (ADR-0171): a second level whose people
-- hear of an alert only when a follow-up step names the schedule, after
-- nobody acknowledged it in time.
ALTER TABLE on_call_schedules
    ADD COLUMN follow_up_only boolean NOT NULL DEFAULT false;
