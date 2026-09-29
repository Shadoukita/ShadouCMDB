-- Schema change records hide their counts from readers who may not view the
-- types they count (GH#252).
--
-- A recorded schema change can say how many values or rows it touched (a
-- purge, a type change, a required field left nullable). Since GH#243 those
-- counts are only recorded for a writer who may view every type concerned, but
-- GET /schema-changes returned them to every user with the data-model right.
-- Each record now also keeps
--
--   * count_classes: the types whose stored data its counts describe (with
--     every type below them). NULL: not known (recorded before this migration);
--   * redacted_summary / redacted_impact: the same record without the counts
--     (NULL: the record carries none).
--
-- A reader who cannot view every type in count_classes (any restricted reader
-- when it is NULL) gets the redacted variant.

ALTER TABLE cmdb.schema_changes
  ADD COLUMN count_classes uuid[],
  ADD COLUMN redacted_summary text,
  ADD COLUMN redacted_impact jsonb,
  ADD CONSTRAINT schema_changes_redacted_impact_array
    CHECK (redacted_impact IS NULL OR jsonb_typeof(redacted_impact) = 'array');
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- The records that already exist: count-free variants of the messages the
-- engine and migrations 0009/0016 wrote. They are not edits of the history.
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.schema_changes DISABLE TRIGGER schema_changes_append_only;
--> statement-breakpoint
WITH redacted AS (
  SELECT s.id,
         regexp_replace(regexp_replace(regexp_replace(s.summary,
           ' \(\d+ CIs, \d+ relationships deleted\)$', ' (its CIs and their relationships deleted)'),
           '^(Migration 0009: )\d+ (attribute values)', '\1\2'),
           ' \(\d+ values\)$', '') AS summary,
         COALESCE((
           SELECT jsonb_agg(CASE
                    WHEN e->>'kind' IN ('drop_column', 'drop_table', 'rewrite', 'warning', 'data_moved', 'not_null_skipped')
                         AND jsonb_typeof(e->'rows') = 'number'
                    THEN e || jsonb_build_object('rows', NULL, 'message',
                           regexp_replace(regexp_replace(regexp_replace(e->>'message',
                             '^\d+ (stored values of|values of|values moved)', 'The \1'),
                             ' and its \d+ rows are deleted$', ' and its rows are deleted'),
                             ': \d+ (assets|CIs)( \(deleted ones included\))? have no value$', ': some \1\2 have no value'))
                    ELSE e END ORDER BY n)
           FROM jsonb_array_elements(s.impact) WITH ORDINALITY AS x(e, n)), '[]'::jsonb) AS impact
  FROM cmdb.schema_changes s
)
UPDATE cmdb.schema_changes s
SET redacted_summary = r.summary,
    redacted_impact = r.impact
FROM redacted r
WHERE r.id = s.id AND (r.summary <> s.summary OR r.impact <> s.impact);
--> statement-breakpoint
ALTER TABLE cmdb.schema_changes ENABLE TRIGGER schema_changes_append_only;
