-- On call together (ADR-0170): a schedule may put several people on call
-- at once. Turn k takes `people_per_turn` members in order from
-- `anchor_position + k * people_per_turn`; a cover replaces one place of
-- the turn, `seat`, 0 being the first person.
ALTER TABLE on_call_schedules
    ADD COLUMN people_per_turn smallint NOT NULL DEFAULT 1
        CHECK (people_per_turn BETWEEN 1 AND 5);
ALTER TABLE on_call_overrides
    ADD COLUMN seat smallint NOT NULL DEFAULT 0 CHECK (seat BETWEEN 0 AND 4);
