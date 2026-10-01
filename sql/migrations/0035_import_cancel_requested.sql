-- Bulk import: a stop requested during a commit (v0.2.0, SHAA-981, GH#359).
--
-- Cancelling a commit that a worker is running no longer sets the job to
-- 'cancelled' at once: the worker's current chunk may still commit, and a job
-- in a final status must not change its counts any more. The cancel records
-- the request here; the job stays 'committing' until the worker ends it as
-- 'cancelled', with finished_at, in the transaction that writes its final
-- counts and its import.commit event. A job taken over by another worker
-- after its lease ran out ends the same way. The timestamp stays on the job as
-- the time of the request.
--
-- A new, empty column; no existing data changes.

ALTER TABLE cmdb.import_jobs ADD COLUMN cancel_requested_at timestamptz;
