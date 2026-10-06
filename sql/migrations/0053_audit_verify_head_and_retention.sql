-- audit_log_verify(): check the chain head, and tell retention from deletion
-- (SHAA-1439, GH#511, GH#512); the backup.restore event (GH#513).
--
-- GH#511. audit_log_verify() compared the head only by sequence number, so
-- someone with the schema owner's rights who rewrote the newest rows and
-- recomputed their hashes with cmdb.audit_log_hash() left a chain that
-- verified. The head (audit_log_chain_head) keeps the hash the trigger wrote;
-- a new `head` finding reports when the row it points to has another row_hash.
-- The row at head.last_seq is compared, not the newest row: inside a
-- transaction that inserted audit rows the head lags until commit (migration
-- 0040), and its row is then the last committed one, which still matches.
--
-- GH#512. Every gap was a `gap`, and `audit-verify --allow-gaps` passed them
-- all, so a recent deletion looked like a prune-audit run. A gap is now
--   retention  an audit.purge row written after the missing rows covers it:
--              its cutoff respects the 30-day floor of prune_audit_log(), and
--              the row before the gap is older than that cutoff (one hour of
--              slack for a transaction that committed after a later-started
--              one; rows that old can be pruned by the owner anyway);
--   deleted    anything else. `audit-verify` fails on it even with --allow-gaps.
-- A forged audit.purge row is itself a chained row: it is either at the end
-- of the chain (and the SIEM copy lacks it) or reported as altered.
--
-- GH#513. `shadoucmdb restore` records a `backup.restore` event (entity type
-- audit_log, actor system) naming the backup and the chain head it restored,
-- so a rolled-back chain shows in the SIEM copy. Like audit.purge it belongs
-- to no prune-audit scope and is never pruned.

-- ---------------------------------------------------------------------------
-- audit_log: the new event (keeps every action up to 0051). Re-added NOT VALID;
-- 0054 validates them in its own transaction (see sql/README.md).
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.audit_log DROP CONSTRAINT audit_log_action_valid;
--> statement-breakpoint
ALTER TABLE cmdb.audit_log ADD CONSTRAINT audit_log_action_valid CHECK (action IN (
  'create', 'update', 'delete', 'restore',
  'login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke',
  'audit.purge', 'token.use',
  'mfa.enrol', 'mfa.disable', 'mfa.failure', 'mfa.recovery_code_used', 'mfa.recovery_codes',
  'schema_change.refused',
  'import.commit', 'import.report_read',
  'export',
  'session.reauthenticate', 'session.reauthentication_required',
  'workflow.publish', 'workflow.start', 'workflow.cancel',
  'workflow.transition', 'workflow.migrate', 'workflow.force',
  'workflow.approval_request', 'workflow.approval_decide', 'workflow.approval_close', 'workflow.approval_overdue',
  'backup.restore'
)) NOT VALID;
--> statement-breakpoint
ALTER TABLE cmdb.audit_log DROP CONSTRAINT audit_log_values_present;
--> statement-breakpoint
ALTER TABLE cmdb.audit_log ADD CONSTRAINT audit_log_values_present CHECK (
  (action = 'create' AND old_value IS NULL AND new_value IS NOT NULL)
  OR (action = 'update' AND old_value IS NOT NULL AND new_value IS NOT NULL)
  OR (action IN ('delete', 'restore') AND old_value IS NOT NULL)
  -- Events, not changes: the details are in new_value.
  OR (action IN ('login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke', 'audit.purge', 'token.use',
                 'mfa.enrol', 'mfa.disable', 'mfa.failure', 'mfa.recovery_code_used', 'mfa.recovery_codes',
                 'schema_change.refused', 'import.commit', 'import.report_read', 'export',
                 'session.reauthenticate', 'session.reauthentication_required',
                 'workflow.publish', 'workflow.start', 'workflow.cancel',
                 'workflow.approval_request', 'workflow.approval_decide', 'workflow.approval_close',
                 'workflow.approval_overdue', 'backup.restore')
      AND old_value IS NULL AND new_value IS NOT NULL)
  -- Workflow steps that change a CI's state: before and after.
  OR (action IN ('workflow.transition', 'workflow.migrate', 'workflow.force')
      AND old_value IS NOT NULL AND new_value IS NOT NULL)
) NOT VALID;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- The cutoff of an audit.purge row (new_value is written by prune_audit_log()
-- only). NULL when it is missing or not a timestamp, so a malformed row is
-- left out instead of failing the whole check.
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.audit_log_purge_cutoff(p_new_value jsonb) RETURNS timestamptz
LANGUAGE plpgsql STABLE
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
  RETURN (p_new_value->>'cutoff')::timestamptz;
EXCEPTION WHEN data_exception THEN
  RETURN NULL;
END;
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- audit_log_verify(): one row per problem; no rows means the chain is intact.
--   altered    the row's content no longer matches its row_hash
--   relinked   prev_hash is not the previous row's row_hash (rows replaced)
--   retention  chain_seq values missing, removed by prune-audit (see above)
--   deleted    chain_seq values missing that no prune-audit run accounts for
--   tail       rows after the last one missing (head is ahead of the table)
--   head       the head's hash is not the row_hash of the row it points to
-- Same signature as 0018, so CREATE OR REPLACE keeps the owner and grants.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION cmdb.audit_log_verify()
RETURNS TABLE (chain_seq bigint, audit_id bigint, problem text, detail text)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
  WITH c AS (
    SELECT a.*,
           lag(a.chain_seq) OVER w AS before_seq,
           lag(a.row_hash) OVER w AS before_hash,
           lag(a.occurred_at) OVER w AS before_at
    FROM cmdb.audit_log a
    WINDOW w AS (ORDER BY a.chain_seq)
  ),
  -- prune-audit runs whose cutoff respects the 30-day floor.
  purge AS (
    SELECT a.chain_seq, cmdb.audit_log_purge_cutoff(a.new_value) AS cutoff
    FROM cmdb.audit_log a
    WHERE a.action = 'audit.purge' AND a.actor_type = 'system'
      AND cmdb.audit_log_purge_cutoff(a.new_value) <= a.occurred_at - interval '30 days'
  ),
  gap AS (
    SELECT c.chain_seq, c.id, coalesce(c.before_seq, 0) + 1 AS first_missing,
           (SELECT p.chain_seq FROM purge p
            WHERE p.chain_seq >= c.chain_seq
              AND (c.before_at IS NULL OR c.before_at < p.cutoff + interval '1 hour')
            ORDER BY p.chain_seq LIMIT 1) AS purge_seq
    FROM c
    WHERE c.chain_seq > coalesce(c.before_seq, 0) + 1
  )
  SELECT c.chain_seq, c.id, 'altered', 'stored row_hash does not match the row content'
  FROM c
  WHERE c.row_hash IS DISTINCT FROM cmdb.audit_log_hash(c.prev_hash, c.chain_seq, c.occurred_at, c.actor_type, c.actor_id,
                                                        c.actor_name, c.action, c.entity_type, c.entity_id, c.old_value,
                                                        c.new_value, c.request_id)
  UNION ALL
  SELECT c.chain_seq, c.id, 'relinked', 'prev_hash does not match the row_hash of row ' || c.before_seq
  FROM c
  WHERE c.chain_seq = c.before_seq + 1 AND c.prev_hash IS DISTINCT FROM c.before_hash
  UNION ALL
  SELECT g.chain_seq, g.id,
         CASE WHEN g.purge_seq IS NULL THEN 'deleted' ELSE 'retention' END,
         CASE WHEN g.purge_seq IS NULL
              THEN format('rows %s to %s missing; no prune-audit run (audit.purge) accounts for them',
                          g.first_missing, g.chain_seq - 1)
              ELSE format('rows %s to %s missing; pruned by the prune-audit run recorded at chainSeq %s',
                          g.first_missing, g.chain_seq - 1, g.purge_seq)
         END
  FROM gap g
  UNION ALL
  SELECT h.last_seq, NULL, 'tail',
         format('rows %s to %s missing', coalesce(m.max_seq, 0) + 1, h.last_seq)
  FROM cmdb.audit_log_chain_head h, (SELECT max(a.chain_seq) AS max_seq FROM cmdb.audit_log a) m
  WHERE h.last_seq > coalesce(m.max_seq, 0)
  UNION ALL
  SELECT h.last_seq, a.id, 'head',
         format('audit_log_chain_head records rowHash %s for this row, the row has %s',
                encode(h.last_hash, 'hex'), encode(a.row_hash, 'hex'))
  FROM cmdb.audit_log_chain_head h JOIN cmdb.audit_log a ON a.chain_seq = h.last_seq
  WHERE a.row_hash IS DISTINCT FROM h.last_hash
  ORDER BY 1
$$;
