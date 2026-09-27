-- audit_log tamper evidence: TRUNCATE is rejected and every row is hash-chained (SHAA-80).
--
-- Each row carries chain_seq (1, 2, 3, ... in commit order), prev_hash (the
-- row_hash of the row before it; 32 zero bytes for the first) and
--   row_hash = sha256(prev_hash || jsonb_build_array(chain_seq, occurred_at,
--              actor_type, actor_id, actor_name, action, entity_type,
--              entity_id, old_value, new_value, request_id)::text)
-- set by a BEFORE INSERT trigger, so every insert path is chained and no
-- caller can choose the values. Changing a row's content breaks its own hash;
-- deleting rows leaves a gap in chain_seq; rewriting the chain from some row
-- onward changes every later hash, including the head that the SIEM export
-- (AUDIT_EXPORT) has already carried off the host. `shadoucmdb audit-verify`
-- runs audit_log_verify() and prints the head to compare with the SIEM copy.
--
-- Inserts are serialised on the single row of audit_log_chain_head, held
-- until the inserting transaction commits: chain order is commit order. The
-- trigger function is SECURITY DEFINER, so the API role needs (and gets) no
-- privilege on the head table.
--
-- Retention (deleting old rows on purpose) shows up as gaps; audit_log_verify
-- reports them separately from altered rows.
--
-- Not reversible once rows are chained: audit_log is append-only.

-- ---------------------------------------------------------------------------
-- TRUNCATE skips row triggers, so it needs its own.
-- ---------------------------------------------------------------------------
DROP TRIGGER IF EXISTS audit_log_no_truncate ON audit_log;
--> statement-breakpoint
CREATE TRIGGER audit_log_no_truncate
  BEFORE TRUNCATE ON audit_log
  FOR EACH STATEMENT EXECUTE FUNCTION audit_log_append_only();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Hash of one row. Timestamps are hashed in UTC with microseconds, so the
-- result does not depend on the session's TimeZone or DateStyle.
-- ---------------------------------------------------------------------------
CREATE FUNCTION audit_log_hash(
  p_prev_hash bytea, p_chain_seq bigint, p_occurred_at timestamptz, p_actor_type text, p_actor_id text,
  p_actor_name text, p_action text, p_entity_type text, p_entity_id uuid, p_old_value jsonb,
  p_new_value jsonb, p_request_id text
) RETURNS bytea
LANGUAGE sql IMMUTABLE PARALLEL SAFE
SET search_path = pg_catalog, pg_temp
AS $$
  SELECT sha256(p_prev_hash || convert_to(jsonb_build_array(
    p_chain_seq,
    to_char(p_occurred_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
    p_actor_type, p_actor_id, p_actor_name, p_action, p_entity_type, p_entity_id::text,
    p_old_value, p_new_value, p_request_id
  )::text, 'UTF8'))
$$;
--> statement-breakpoint

CREATE TABLE audit_log_chain_head (
  singleton boolean PRIMARY KEY DEFAULT true,
  last_seq bigint NOT NULL,
  last_hash bytea NOT NULL,
  CONSTRAINT audit_log_chain_head_singleton CHECK (singleton),
  CONSTRAINT audit_log_chain_head_hash_len CHECK (octet_length(last_hash) = 32)
);
--> statement-breakpoint
REVOKE ALL ON audit_log_chain_head FROM PUBLIC;
--> statement-breakpoint

ALTER TABLE audit_log ADD COLUMN chain_seq bigint, ADD COLUMN prev_hash bytea, ADD COLUMN row_hash bytea;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Chain the rows that already exist, in id order.
-- ---------------------------------------------------------------------------
ALTER TABLE audit_log DISABLE TRIGGER audit_log_append_only;
--> statement-breakpoint
DO $$
DECLARE
  r audit_log;
  seq bigint := 0;
  prev bytea := decode(repeat('00', 32), 'hex');
  h bytea;
BEGIN
  FOR r IN SELECT * FROM audit_log ORDER BY id LOOP
    seq := seq + 1;
    h := audit_log_hash(prev, seq, r.occurred_at, r.actor_type, r.actor_id, r.actor_name, r.action,
                        r.entity_type, r.entity_id, r.old_value, r.new_value, r.request_id);
    UPDATE audit_log SET chain_seq = seq, prev_hash = prev, row_hash = h WHERE id = r.id;
    prev := h;
  END LOOP;
  INSERT INTO audit_log_chain_head (last_seq, last_hash) VALUES (seq, prev);
END;
$$;
--> statement-breakpoint
ALTER TABLE audit_log ENABLE TRIGGER audit_log_append_only;
--> statement-breakpoint

ALTER TABLE audit_log
  ALTER COLUMN chain_seq SET NOT NULL,
  ALTER COLUMN prev_hash SET NOT NULL,
  ALTER COLUMN row_hash SET NOT NULL,
  ADD CONSTRAINT audit_log_chain_seq_unique UNIQUE (chain_seq);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- New rows: next link in the chain. Runs after column defaults, so
-- occurred_at is final; whatever the caller put in the chain columns is overwritten.
-- ---------------------------------------------------------------------------
CREATE FUNCTION audit_log_chain() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER
SET search_path FROM CURRENT
AS $$
DECLARE
  head audit_log_chain_head;
BEGIN
  SELECT * INTO STRICT head FROM audit_log_chain_head FOR UPDATE;
  NEW.chain_seq := head.last_seq + 1;
  NEW.prev_hash := head.last_hash;
  NEW.row_hash := audit_log_hash(NEW.prev_hash, NEW.chain_seq, NEW.occurred_at, NEW.actor_type, NEW.actor_id,
                                 NEW.actor_name, NEW.action, NEW.entity_type, NEW.entity_id, NEW.old_value,
                                 NEW.new_value, NEW.request_id);
  UPDATE audit_log_chain_head SET last_seq = NEW.chain_seq, last_hash = NEW.row_hash;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
REVOKE ALL ON FUNCTION audit_log_chain() FROM PUBLIC;
--> statement-breakpoint
CREATE TRIGGER audit_log_chain
  BEFORE INSERT ON audit_log
  FOR EACH ROW EXECUTE FUNCTION audit_log_chain();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- audit_log_verify(): one row per problem; no rows means the chain is intact.
--   altered  the row's content no longer matches its row_hash
--   relinked prev_hash is not the previous row's row_hash (rows replaced)
--   gap      chain_seq values missing before this row (deleted or pruned)
--   tail     rows after the last one missing (head is ahead of the table)
-- SECURITY DEFINER so a verifier needs no privilege on the head table.
-- ---------------------------------------------------------------------------
CREATE FUNCTION audit_log_verify()
RETURNS TABLE (chain_seq bigint, audit_id bigint, problem text, detail text)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path FROM CURRENT
AS $$
  WITH c AS (
    SELECT a.*,
           lag(a.chain_seq) OVER w AS before_seq,
           lag(a.row_hash) OVER w AS before_hash
    FROM audit_log a
    WINDOW w AS (ORDER BY a.chain_seq)
  )
  SELECT c.chain_seq, c.id, 'altered', 'stored row_hash does not match the row content'
  FROM c
  WHERE c.row_hash IS DISTINCT FROM audit_log_hash(c.prev_hash, c.chain_seq, c.occurred_at, c.actor_type, c.actor_id,
                                                   c.actor_name, c.action, c.entity_type, c.entity_id, c.old_value,
                                                   c.new_value, c.request_id)
  UNION ALL
  SELECT c.chain_seq, c.id, 'relinked', 'prev_hash does not match the row_hash of row ' || c.before_seq
  FROM c
  WHERE c.chain_seq = c.before_seq + 1 AND c.prev_hash IS DISTINCT FROM c.before_hash
  UNION ALL
  SELECT c.chain_seq, c.id, 'gap',
         format('rows %s to %s missing', coalesce(c.before_seq, 0) + 1, c.chain_seq - 1)
  FROM c
  WHERE c.chain_seq > coalesce(c.before_seq, 0) + 1
  UNION ALL
  SELECT h.last_seq, NULL, 'tail',
         format('rows %s to %s missing', coalesce(m.max_seq, 0) + 1, h.last_seq)
  FROM audit_log_chain_head h, (SELECT max(a.chain_seq) AS max_seq FROM audit_log a) m
  WHERE h.last_seq > coalesce(m.max_seq, 0)
  ORDER BY 1
$$;
