-- Test sends per user and minute (GH#899, GH#904): the workflow action test
-- (`POST .../actions/{key}/test`) and the mail test (`POST
-- /admin/mail/test`) each allow 10 a minute.
--
-- The action test used to count its audit entries, which are written only
-- after the send: concurrent requests all passed the count. A request now
-- takes a slot here before it sends, in a transaction that holds a per-user
-- advisory lock while it counts and inserts, so the limit holds across
-- concurrent requests and server processes. A user keeps at most the slots of
-- the last minute: taking one deletes their expired ones.

CREATE TABLE cmdb.test_send_slots (
  id       bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  user_id  uuid NOT NULL REFERENCES cmdb.users (id) ON DELETE CASCADE,
  kind     text NOT NULL CHECK (kind IN ('workflow_action', 'mail')),
  taken_at timestamptz NOT NULL DEFAULT now()
);
--> statement-breakpoint
CREATE INDEX test_send_slots_user_idx ON cmdb.test_send_slots (user_id, kind, taken_at);
