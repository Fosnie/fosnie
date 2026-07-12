-- Tier-2 #16: calendar reminders. The scheduler pushes a lookahead reminder to
-- an automation's owner shortly before it is due. `reminded_for` records the
-- `next_run_at` value already reminded about, so each occurrence is alerted at
-- most once (it is reset implicitly when next_run_at advances to a new value).
ALTER TABLE automations ADD COLUMN reminded_for TIMESTAMPTZ;
