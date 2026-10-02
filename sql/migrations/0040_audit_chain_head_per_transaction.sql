-- Audit hash chain: move the head once per transaction, not once per row (SHAA-1371, GH#487).
--
-- Migration 0018's trigger updated the single row of audit_log_chain_head for
-- every audit row. Inside one transaction each update leaves a row version
-- that cannot be pruned until the transaction ends, and every later lock and
-- update walks all of them: N audit rows in one transaction cost O(N^2). A
-- large configuration import held its advisory lock and a connection for
-- minutes.
--
-- Now:
--   * audit_log_chain() still locks the head FOR UPDATE on every insert. A
--     row we already hold the lock on is not written again, so this adds no
--     row version, and chain order is still commit order: a concurrent
--     transaction waits on the lock until this one commits, as before.
--   * It continues the chain from the newest of (a) the head as committed by
--     the previous transaction and (b) the newest audit_log row this
--     transaction can see (a lookup on the chain_seq unique index). While we
--     hold the lock nobody else can commit audit rows, so (b) is ahead of (a)
--     only through rows this transaction inserted. Rows deleted from the end
--     of the chain leave (b) behind (a), and the chain continues from the
--     head, so the deletion still shows as a gap, exactly as before.
--   * audit_log_chain_commit(), a deferred constraint trigger, writes the
--     head back at commit. The first firing moves it to the newest row; every
--     later firing in the same commit finds it current and writes nothing.
--     The head therefore moves once per transaction, before the lock is
--     released, and REPEATABLE READ / SERIALIZABLE writers still get a
--     serialization failure on the head instead of a forked chain.
--
-- Hash, columns, audit_log_verify() and the head table are unchanged, so
-- existing chains verify as before and no row is rewritten. Within the
-- inserting transaction the head lags the rows until commit; nothing reads it
-- there (backup, audit-verify and the SIEM export run in their own
-- transactions). Rolling back to a savepoint drops its rows and leaves the
-- head where it was, so the chain continues from the last surviving row.
--
-- Both functions are SECURITY DEFINER: the API role keeps only SELECT on the
-- head (migration 0038), and cannot drop or disable the commit trigger, which
-- it does not own. SET CONSTRAINTS ... IMMEDIATE only makes the trigger fire
-- after each statement instead of at commit: slower, never wrong.

CREATE OR REPLACE FUNCTION cmdb.audit_log_chain() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
  head cmdb.audit_log_chain_head;
  last_seq bigint;
  last_hash bytea;
BEGIN
  SELECT * INTO STRICT head FROM cmdb.audit_log_chain_head FOR UPDATE;
  SELECT a.chain_seq, a.row_hash INTO last_seq, last_hash
  FROM cmdb.audit_log a ORDER BY a.chain_seq DESC LIMIT 1;
  IF last_seq IS NULL OR last_seq <= head.last_seq THEN
    last_seq := head.last_seq;
    last_hash := head.last_hash;
  END IF;
  NEW.chain_seq := last_seq + 1;
  NEW.prev_hash := last_hash;
  NEW.row_hash := cmdb.audit_log_hash(NEW.prev_hash, NEW.chain_seq, NEW.occurred_at, NEW.actor_type, NEW.actor_id,
                                      NEW.actor_name, NEW.action, NEW.entity_type, NEW.entity_id, NEW.old_value,
                                      NEW.new_value, NEW.request_id);
  RETURN NEW;
END;
$$;
--> statement-breakpoint

CREATE FUNCTION cmdb.audit_log_chain_commit() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
  UPDATE cmdb.audit_log_chain_head h SET last_seq = a.chain_seq, last_hash = a.row_hash
  FROM (SELECT chain_seq, row_hash FROM cmdb.audit_log ORDER BY chain_seq DESC LIMIT 1) a
  WHERE a.chain_seq > h.last_seq;
  RETURN NULL;
END;
$$;
--> statement-breakpoint
REVOKE ALL ON FUNCTION cmdb.audit_log_chain_commit() FROM PUBLIC;
--> statement-breakpoint
CREATE CONSTRAINT TRIGGER audit_log_chain_commit
  AFTER INSERT ON cmdb.audit_log
  DEFERRABLE INITIALLY DEFERRED
  FOR EACH ROW EXECUTE FUNCTION cmdb.audit_log_chain_commit();
