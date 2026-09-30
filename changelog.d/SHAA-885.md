### Added: impact analysis, and a Criticality field on every CI

Every CI can now be analysed for **impact**: which CIs are affected if it fails or changes
(downstream) and which CIs it depends on (upstream), with the hop count and the shortest path to
each, grouped by class, criticality and distance, and exportable as CSV. New API:
`GET /api/v1/configuration-items/{id}/impact`, `…/impact/export` (CSV) and
`GET /api/v1/settings/impact`; the web UI follows in a later change.

- Administrators with *Manage data model* choose per relationship type whether and which way it
  propagates impact (`impactDirection`: `none`, `target_to_source`, `source_to_target`, `both`;
  a non-directional type only `none` or `both`). Changes are audited like any relationship type
  update.
- Results contain only CIs the user may view, and an analysis never walks through a CI the user
  may not view: a CI reachable only through one is left out. A user whose profile limits the
  classes they may view sees `visibility: restricted` on every result.
- Analyses are bounded (default 10 hops, 2 000 CIs, 5 relationships per CI read, 5 seconds) and
  capped per server process at 8 running at once (at most half of `DATABASE_POOL_MAX`, so 5 with
  the default pool of 10) and 2 per user; see the `IMPACT_*` settings in `.env.example`. A bounded
  result is returned marked `truncated`. The server refuses to start if `IMPACT_MAX_CONCURRENT` is
  above half of `DATABASE_POOL_MAX`.
- CSV exports are recorded in the audit log (new action `export`, with the parameters and the row
  count, never the rows) and neutralise spreadsheet formulas. `prune-audit --scope changes` covers
  them; the default `auth` scope does not.
- New optional core field **Criticality** on every CI (`criticalityValueId` on create and update,
  `criticality` on every CI read, list filter `criticalityValueId`, sort `criticality`), recorded
  in the CI history. Its values are the new system lookup list *Criticality* (critical, high,
  medium, low), which can be renamed, reordered and extended but not deleted
  (`systemRole: criticality` on the list).
- The configuration export format is now version 4 (`impactDirection` on relationship types,
  `systemRole` on lookup lists); files of versions 1 to 3 still import, and leave the impact
  direction of existing types unchanged.

**Upgrade:** migrations 0029 to 0031 run on start; they are additive and need no downtime. On
upgrade, the starter types *runs on*, *depends on* and *located in* are set to propagate impact
(target to source) if their labels are still the starter template's; every other type starts at
`none`. **Review your relationship types after upgrading** (*Data model → Relationship types*),
otherwise impact analysis finds nothing across them. If a lookup list keyed `criticality` already
exists, the new system list is keyed `criticality_2`. Going back to 0.2.x after the migrations
is not supported: restore the backup taken before the upgrade.
