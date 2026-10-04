-- The instance list's two sort orders (SHAA-1424): newest activity first
-- (the default) and by start time, across every workflow. Without them a deep
-- page sorts all instances; with them it walks the index. The table is new in
-- 0046 and empty on an upgrade, so a plain CREATE INDEX blocks nothing.

CREATE INDEX workflow_instances_recent_idx ON cmdb.workflow_instances (last_transition_at, id);
--> statement-breakpoint
CREATE INDEX workflow_instances_started_idx ON cmdb.workflow_instances (started_at, id);
