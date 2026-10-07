-- audit_log_verify(): a gap is `retention` only when prune-audit runs record
-- exactly the rows it lacks (SHAA-2177, GH#683, GH#684).
--
-- 0053 accepted a gap when a later audit.purge row had a cutoff past the row
-- before the gap. Two ways around it:
--   GH#683  a gap at the start of the chain has no row before it, and any
--           later purge excused it, even one that deleted nothing; and a
--           prune that removed old rows also excused recent rows deleted
--           next to them.
--   GH#684  the API role holds INSERT on audit_log, so it could write a
--           validly chained audit.purge row of its own, dated in the future
--           with a cutoff in the future, which excused every gap before it.
--
-- Now:
--   - prune_audit_log() records the chainSeq ranges it deleted in its
--     audit.purge row (`deletedRanges`, [[first, last], ...]).
--   - Only the owner of audit_log may insert an audit.purge row: inside
--     prune_audit_log() (SECURITY DEFINER), in a migration, or by someone with
--     the owner's rights, who could delete rows anyway. The API role and the
--     maintenance role get insufficient_privilege. The API role keeps INSERT
--     on audit_log for every other action. On a single-role install the API
--     role is the owner, so this split does not apply there.
--   - A gap is `retention` only when every missing chainSeq is in the
--     deletedRanges of an audit.purge row that
--       * is written by the system, its hash matches its content,
--       * is not dated in the future, nor more than an hour after the row
--         that follows it in the chain,
--       * has a cutoff at least 30 days before its own date,
--       * records ranges that lie before it in the chain and add up to the
--         number of rows its `deleted` counts,
--     and the row before the gap, if any, is older than the cutoff of each
--     such run (one hour of slack, as in 0053). Anything else is `deleted`.
--
-- Purge rows written before this migration carry no ranges. So that existing
-- retention gaps keep verifying, this migration writes one audit.purge row
-- (`scope` legacy) recording the gaps 0053 accepted when it ran, apart from
-- those only a future-dated purge row excused. Gaps 0053 reported as
-- `deleted` stay so, and a deletion after the upgrade, of old rows as well as
-- new ones, is `deleted` unless a later prune-audit run records it. Review
-- the legacy row's ranges against the SIEM copy (AUDIT_EXPORT) if you have
-- reason to doubt the history before the upgrade.

-- ---------------------------------------------------------------------------
-- The legacy row: gaps 0053 accepted, with the purge rows it trusted, now
-- that they can no longer be dated in the future. Written before the insert
-- guard below exists; the migration runs as the owner either way.
-- ---------------------------------------------------------------------------
DO $$
DECLARE
  ranges jsonb;
BEGIN
  WITH c AS (
    SELECT a.chain_seq, lag(a.chain_seq) OVER w AS before_seq, lag(a.occurred_at) OVER w AS before_at
    FROM cmdb.audit_log a
    WINDOW w AS (ORDER BY a.chain_seq)
  ),
  purge AS (
    SELECT a.chain_seq, cmdb.audit_log_purge_cutoff(a.new_value) AS cutoff
    FROM cmdb.audit_log a
    WHERE a.action = 'audit.purge' AND a.actor_type = 'system' AND a.occurred_at <= now()
      AND cmdb.audit_log_purge_cutoff(a.new_value) <= a.occurred_at - interval '30 days'
  )
  SELECT jsonb_agg(jsonb_build_array(coalesce(c.before_seq, 0) + 1, c.chain_seq - 1) ORDER BY c.chain_seq)
  INTO ranges
  FROM c
  WHERE c.chain_seq > coalesce(c.before_seq, 0) + 1
    AND EXISTS (SELECT FROM purge p
                WHERE p.chain_seq >= c.chain_seq
                  AND (c.before_at IS NULL OR c.before_at < p.cutoff + interval '1 hour'));

  IF ranges IS NOT NULL THEN
    INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value)
    VALUES ('system', session_user, 'audit.purge', 'audit_log', gen_random_uuid(), jsonb_build_object(
      'scope', 'legacy',
      'deletedRanges', ranges,
      'migration', '0060',
      'note', 'Gaps left by prune-audit runs before this upgrade, which recorded no ranges',
      'databaseUser', session_user
    ));
  END IF;
END $$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Only the owner of audit_log writes audit.purge rows (GH#684). Not SECURITY
-- DEFINER, so current_user is the role inserting: the owner inside
-- prune_audit_log(), the API role for a direct INSERT.
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.audit_log_purge_owner_only() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
  IF NEW.action = 'audit.purge' AND current_user <> (
    SELECT r.rolname FROM pg_class c JOIN pg_roles r ON r.oid = c.relowner WHERE c.oid = TG_RELID
  ) THEN
    RAISE EXCEPTION 'audit_log: audit.purge entries are written by prune_audit_log() only'
      USING ERRCODE = 'insufficient_privilege';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER audit_log_purge_owner_only
  BEFORE INSERT ON cmdb.audit_log
  FOR EACH ROW EXECUTE FUNCTION cmdb.audit_log_purge_owner_only();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- The chainSeq values an audit.purge row accounts for, or NULL when its
-- deletedRanges are missing or malformed, reach past the row itself, or (for
-- a prune-audit run) do not add up to the rows it counts as deleted.
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.audit_log_purge_ranges(p_new_value jsonb, p_chain_seq bigint) RETURNS int8multirange
LANGUAGE plpgsql STABLE
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
  ranges int8multirange;
  total bigint;
  deleted bigint;
BEGIN
  IF jsonb_typeof(p_new_value->'deletedRanges') IS DISTINCT FROM 'array' THEN
    RETURN NULL;
  END IF;
  IF EXISTS (SELECT FROM jsonb_array_elements(p_new_value->'deletedRanges') r
             WHERE jsonb_typeof(r) <> 'array' OR jsonb_array_length(r) <> 2
                OR (r->>0)::bigint < 1 OR (r->>0)::bigint > (r->>1)::bigint OR (r->>1)::bigint >= p_chain_seq) THEN
    RETURN NULL;
  END IF;
  SELECT coalesce(range_agg(int8range((r->>0)::bigint, (r->>1)::bigint, '[]')), '{}'),
         coalesce(sum((r->>1)::bigint - (r->>0)::bigint + 1), 0)
  INTO ranges, total
  FROM jsonb_array_elements(p_new_value->'deletedRanges') r;
  IF p_new_value->>'scope' IS DISTINCT FROM 'legacy' THEN
    SELECT coalesce(sum(d.value::bigint), 0) INTO deleted FROM jsonb_each_text(p_new_value->'deleted') d;
    -- Overlapping ranges would count rows twice: the merged ranges must hold as many.
    IF deleted <> total OR total <> (SELECT coalesce(sum(upper(u) - lower(u)), 0) FROM unnest(ranges) u) THEN
      RETURN NULL;
    END IF;
  END IF;
  RETURN ranges;
EXCEPTION WHEN data_exception THEN
  RETURN NULL;
END;
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- prune_audit_log(): records the ranges it deleted. Same body as 0056
-- otherwise; CREATE OR REPLACE keeps the owner and the EXECUTE grants.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION cmdb.prune_audit_log(p_older_than interval, p_scope text, p_dry_run boolean, p_operator text DEFAULT NULL)
RETURNS TABLE (category text, total bigint)
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
  cutoff timestamptz := now() - p_older_than;
  session_cutoff timestamptz := now() - interval '30 days';
  actions text[];
  counts jsonb;
  ranges jsonb := '[]';
  sessions_count bigint := 0;
BEGIN
  IF p_older_than IS NULL OR cutoff > now() - interval '30 days' THEN
    RAISE EXCEPTION 'prune_audit_log: the window must be at least 30 days (got %)', p_older_than
      USING ERRCODE = 'invalid_parameter_value';
  END IF;
  actions := CASE p_scope
    WHEN 'auth' THEN ARRAY['login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke', 'token.use',
                           'mfa.enrol', 'mfa.disable', 'mfa.failure', 'mfa.recovery_code_used', 'mfa.recovery_codes',
                           'import.report_read', 'session.reauthenticate', 'session.reauthentication_required']
    WHEN 'changes' THEN ARRAY['create', 'update', 'delete', 'restore', 'schema_change.refused', 'import.commit', 'export',
                              'workflow.publish', 'workflow.start', 'workflow.cancel',
                              'workflow.transition', 'workflow.migrate', 'workflow.force',
                              'workflow.approval_request', 'workflow.approval_decide', 'workflow.approval_close',
                              'workflow.approval_overdue', 'workflow.approval_refresh']
  END;
  IF actions IS NULL OR p_dry_run IS NULL THEN
    RAISE EXCEPTION 'prune_audit_log: scope must be auth or changes, and dry_run true or false'
      USING ERRCODE = 'invalid_parameter_value';
  END IF;

  IF p_dry_run THEN
    SELECT coalesce(jsonb_object_agg(a.action, a.n), '{}') INTO counts
    FROM (SELECT l.action, count(*) AS n FROM cmdb.audit_log l
          WHERE l.action = ANY (actions) AND l.occurred_at < cutoff GROUP BY l.action) a;
    IF p_scope = 'auth' THEN
      SELECT count(*) INTO sessions_count FROM cmdb.sessions s WHERE s.expires_at < session_cutoff;
    END IF;
  ELSE
    PERFORM set_config('shadoucmdb.audit_purge', 'on', true);
    -- Consecutive deleted chainSeq values become one [first, last] range.
    WITH gone AS (
      DELETE FROM cmdb.audit_log l WHERE l.action = ANY (actions) AND l.occurred_at < cutoff
      RETURNING l.action, l.chain_seq
    ),
    island AS (
      SELECT min(i.chain_seq) AS first_seq, max(i.chain_seq) AS last_seq
      FROM (SELECT gone.chain_seq, gone.chain_seq - row_number() OVER (ORDER BY gone.chain_seq) AS grp FROM gone) i
      GROUP BY i.grp
    )
    SELECT (SELECT coalesce(jsonb_object_agg(g.action, g.n), '{}')
            FROM (SELECT gone.action, count(*) AS n FROM gone GROUP BY gone.action) g),
           (SELECT coalesce(jsonb_agg(jsonb_build_array(island.first_seq, island.last_seq) ORDER BY island.first_seq), '[]')
            FROM island)
    INTO counts, ranges;
    PERFORM set_config('shadoucmdb.audit_purge', '', true);
    IF p_scope = 'auth' THEN
      DELETE FROM cmdb.sessions s WHERE s.expires_at < session_cutoff;
      GET DIAGNOSTICS sessions_count = ROW_COUNT;
      DELETE FROM cmdb.mfa_challenges c WHERE c.expires_at < now();
    END IF;

    INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value)
    VALUES ('system', session_user, 'audit.purge', 'audit_log', gen_random_uuid(), jsonb_build_object(
      'scope', p_scope,
      'olderThan', p_older_than::text,
      'cutoff', cutoff,
      'deleted', counts,
      'deletedRanges', ranges,
      'sessionsDeleted', sessions_count,
      'sessionsCutoff', CASE WHEN p_scope = 'auth' THEN session_cutoff END,
      'databaseUser', session_user,
      'clientAddress', host(inet_client_addr()),
      'operator', left(p_operator, 128)
    ));
  END IF;

  RETURN QUERY SELECT c.key, c.value::bigint FROM jsonb_each_text(counts) c ORDER BY c.key;
  IF p_scope = 'auth' THEN
    RETURN QUERY SELECT 'sessions'::text, sessions_count;
  END IF;
END;
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- audit_log_verify(): same findings as 0053; `retention` as described above.
-- Same signature, so CREATE OR REPLACE keeps the owner and grants.
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
           lag(a.occurred_at) OVER w AS before_at,
           lead(a.occurred_at) OVER w AS after_at,
           a.row_hash IS DISTINCT FROM cmdb.audit_log_hash(a.prev_hash, a.chain_seq, a.occurred_at, a.actor_type,
                                                           a.actor_id, a.actor_name, a.action, a.entity_type,
                                                           a.entity_id, a.old_value, a.new_value, a.request_id)
             AS is_altered
    FROM cmdb.audit_log a
    WINDOW w AS (ORDER BY a.chain_seq)
  ),
  -- prune-audit runs that may account for rows (see the header).
  purge AS (
    SELECT p.chain_seq, p.cutoff, p.ranges
    FROM (SELECT c.chain_seq, c.occurred_at,
                 CASE WHEN c.new_value->>'scope' IS DISTINCT FROM 'legacy'
                      THEN cmdb.audit_log_purge_cutoff(c.new_value) END AS cutoff,
                 c.new_value->>'scope' = 'legacy' AS legacy,
                 cmdb.audit_log_purge_ranges(c.new_value, c.chain_seq) AS ranges
          FROM c
          WHERE c.action = 'audit.purge' AND c.actor_type = 'system' AND NOT c.is_altered
            AND c.occurred_at <= now()
            AND (c.after_at IS NULL OR c.occurred_at <= c.after_at + interval '1 hour')) p
    WHERE p.ranges IS NOT NULL
      AND (p.legacy OR p.cutoff <= p.occurred_at - interval '30 days')
  ),
  covered AS (
    SELECT coalesce(range_agg(r), '{}') AS seqs FROM purge, unnest(purge.ranges) r
  ),
  gap AS (
    SELECT c.chain_seq, c.id, c.before_at, coalesce(c.before_seq, 0) + 1 AS first_missing
    FROM c
    WHERE c.chain_seq > coalesce(c.before_seq, 0) + 1
  ),
  -- The runs that removed rows of each gap: a range belongs to the last gap
  -- starting at or before it (gaps and ranges are both ordered by chainSeq).
  run AS (
    SELECT max(x.gap_seq) OVER (ORDER BY x.pos, x.is_range ROWS UNBOUNDED PRECEDING) AS gap_seq,
           x.is_range, x.purge_seq, x.cutoff
    FROM (SELECT g.first_missing AS pos, false AS is_range, g.chain_seq AS gap_seq,
                 NULL::bigint AS purge_seq, NULL::timestamptz AS cutoff
          FROM gap g
          UNION ALL
          SELECT lower(r), true, NULL, p.chain_seq, p.cutoff FROM purge p, unnest(p.ranges) r) x
  ),
  runs AS (
    SELECT run.gap_seq, string_agg(DISTINCT run.purge_seq::text, ', ') AS purge_seqs, min(run.cutoff) AS cutoff
    FROM run
    WHERE run.is_range AND run.gap_seq IS NOT NULL
    GROUP BY run.gap_seq
  ),
  judged AS (
    SELECT g.chain_seq, g.id, g.first_missing, r.purge_seqs,
           int8range(g.first_missing, g.chain_seq) <@ (SELECT seqs FROM covered)
             AND (g.before_at IS NULL OR r.cutoff IS NULL OR g.before_at < r.cutoff + interval '1 hour') AS is_retention
    FROM gap g LEFT JOIN runs r ON r.gap_seq = g.chain_seq
  )
  SELECT c.chain_seq, c.id, 'altered', 'stored row_hash does not match the row content'
  FROM c
  WHERE c.is_altered
  UNION ALL
  SELECT c.chain_seq, c.id, 'relinked', 'prev_hash does not match the row_hash of row ' || c.before_seq
  FROM c
  WHERE c.chain_seq = c.before_seq + 1 AND c.prev_hash IS DISTINCT FROM c.before_hash
  UNION ALL
  SELECT j.chain_seq, j.id,
         CASE WHEN j.is_retention THEN 'retention' ELSE 'deleted' END,
         CASE WHEN j.is_retention
              THEN format('rows %s to %s missing; pruned by the prune-audit run recorded at chainSeq %s',
                          j.first_missing, j.chain_seq - 1, j.purge_seqs)
              ELSE format('rows %s to %s missing; no prune-audit run (audit.purge) accounts for them',
                          j.first_missing, j.chain_seq - 1)
         END
  FROM judged j
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
