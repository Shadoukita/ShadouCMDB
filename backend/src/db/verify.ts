import { fileURLToPath } from 'node:url';
import type pg from 'pg';
import { loadEnv } from '../config/env.js';
import { createPool } from './client.js';

/*
 * Schema acceptance checks against a migrated + seeded database. Everything runs
 * inside one transaction that is ROLLED BACK, so this is safe to point at any
 * environment: it leaves no rows behind.
 *
 *   npm run db:verify
 */

type Check = { name: string; run: (c: pg.PoolClient) => Promise<string> };

async function one<T extends pg.QueryResultRow>(c: pg.PoolClient, text: string, params: unknown[] = []): Promise<T> {
  const r = await c.query<T>(text, params);
  if (!r.rows[0]) throw new Error(`no row: ${text}`);
  return r.rows[0];
}

/** Runs fn in a savepoint and expects it to fail with the given constraint name. */
async function expectReject(c: pg.PoolClient, constraint: string, fn: () => Promise<unknown>): Promise<string> {
  await c.query('SAVEPOINT sp');
  try {
    await fn();
  } catch (err) {
    await c.query('ROLLBACK TO SAVEPOINT sp');
    const e = err as { constraint?: string; code?: string; message: string };
    if (e.constraint === constraint || e.message.includes(constraint)) return `rejected (${e.code} ${constraint})`;
    throw new Error(`expected ${constraint}, got ${e.code} ${e.constraint ?? ''}: ${e.message}`);
  }
  throw new Error(`expected ${constraint} violation, but statement succeeded`);
}

const id = (c: pg.PoolClient, table: string, key: string) =>
  one<{ id: string }>(c, `SELECT id FROM ${table} WHERE key = $1`, [key]).then((r) => r.id);

const newCi = async (c: pg.PoolClient, classKey: string, name: string) =>
  (
    await one<{ id: string }>(
      c,
      `INSERT INTO configuration_items (class_id, name, status_id)
       VALUES ((SELECT id FROM ci_classes WHERE key = $1), $2, (SELECT id FROM statuses WHERE key = 'in_service'))
       RETURNING id`,
      [classKey, name],
    )
  ).id;

const link = (c: pg.PoolClient, type: string, src: string, tgt: string) =>
  c.query(
    `INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id)
     VALUES ((SELECT id FROM relationship_types WHERE key = $1), $2, $3)`,
    [type, src, tgt],
  );

const checks: Check[] = [
  {
    name: 'New CI class with custom attributes is pure data entry',
    run: async (c) => {
      const parent = await id(c, 'ci_classes', 'network_device');
      const lb = await one<{ id: string }>(
        c,
        `INSERT INTO ci_classes (key, name, parent_id) VALUES ('load_balancer', 'Load balancer', $1) RETURNING id`,
        [parent],
      );
      const vip = await one<{ id: string }>(
        c,
        `INSERT INTO ci_attribute_definitions (class_id, key, label, data_type) VALUES ($1, 'vip', 'Virtual IP', 'ip') RETURNING id`,
        [lb.id],
      );
      const algo = await one<{ id: string }>(
        c,
        `INSERT INTO ci_attribute_definitions (class_id, key, label, data_type, enum_values)
         VALUES ($1, 'algorithm', 'Algorithm', 'enum', '["round_robin","least_conn"]') RETURNING id`,
        [lb.id],
      );
      const ci = await newCi(c, 'load_balancer', 'fra1-lb-01');
      const role = await one<{ id: string }>(
        c,
        `SELECT d.id FROM ci_attribute_definitions d JOIN ci_classes k ON k.id = d.class_id
         WHERE k.key = 'network_device' AND d.key = 'device_role'`,
      );
      await c.query(
        `INSERT INTO ci_attribute_values (ci_id, attribute_id, value_ip) VALUES ($1, $2, '192.0.2.10')`,
        [ci, vip.id],
      );
      await c.query(`INSERT INTO ci_attribute_values (ci_id, attribute_id, value_text) VALUES ($1, $2, 'least_conn')`, [ci, algo.id]);
      // Inherited from network_device, which inherits from hardware.
      await c.query(`INSERT INTO ci_attribute_values (ci_id, attribute_id, value_text) VALUES ($1, $2, 'load_balancer')`, [ci, role.id]);
      const attrs = await c.query<{ key: string; depth: number }>(
        `SELECT d.key, l.depth FROM ci_class_lineage($1) l JOIN ci_attribute_definitions d ON d.class_id = l.class_id ORDER BY l.depth, d.sort_order, d.key`,
        [lb.id],
      );
      // Inherited relationship rules apply too: a load balancer is hardware, so it can be located_in a location.
      const rack = await one<{ id: string }>(c, `SELECT id FROM configuration_items WHERE name = 'FRA1 Rack A01'`);
      await link(c, 'located_in', ci, rack.id);
      return `class load_balancer + 2 attributes inserted; effective attributes: ${attrs.rows.map((a) => a.key).join(', ')}; located_in rack accepted via inherited rule`;
    },
  },
  {
    name: 'Server -> Application -> Database and Device -> Location are expressible',
    run: async (c) => {
      const srv = await newCi(c, 'server', 'verify-srv');
      const app = await newCi(c, 'application', 'verify-app');
      const db = await newCi(c, 'database', 'verify-db');
      const dev = await newCi(c, 'network_device', 'verify-switch');
      const loc = await newCi(c, 'location', 'verify-room');
      await link(c, 'runs_on', app, srv);
      await link(c, 'depends_on', app, db);
      await link(c, 'runs_on', db, srv);
      await link(c, 'located_in', dev, loc);
      const r = await c.query<{ path: string }>(
        `SELECT s.name || ' <-runs_on- ' || a.name || ' -depends_on-> ' || d.name AS path
         FROM ci_relationships r1
         JOIN relationship_types t1 ON t1.id = r1.relationship_type_id AND t1.key = 'runs_on'
         JOIN ci_relationships r2 ON r2.source_ci_id = r1.source_ci_id
         JOIN relationship_types t2 ON t2.id = r2.relationship_type_id AND t2.key = 'depends_on'
         JOIN configuration_items s ON s.id = r1.target_ci_id
         JOIN configuration_items a ON a.id = r1.source_ci_id
         JOIN configuration_items d ON d.id = r2.target_ci_id
         WHERE a.id = $1`,
        [app],
      );
      return r.rows.map((x) => x.path).join('; ') + '; verify-switch -located_in-> verify-room';
    },
  },
  {
    name: 'Demo graph traversal (recursive, downstream of the CRM service)',
    run: async (c) => {
      const r = await c.query<{ line: string }>(
        `WITH RECURSIVE g AS (
           SELECT r.source_ci_id, r.target_ci_id, r.relationship_type_id, 1 AS depth
           FROM ci_relationships r JOIN configuration_items ci ON ci.id = r.source_ci_id
           WHERE ci.name = 'Customer Relationship Management' AND r.deleted_at IS NULL
           UNION
           SELECT r.source_ci_id, r.target_ci_id, r.relationship_type_id, g.depth + 1
           FROM ci_relationships r JOIN g ON r.source_ci_id = g.target_ci_id
           WHERE r.deleted_at IS NULL AND g.depth < 6
         )
         SELECT s.name || ' ' || t.forward_label || ' ' || d.name AS line
         FROM g JOIN configuration_items s ON s.id = g.source_ci_id
         JOIN configuration_items d ON d.id = g.target_ci_id
         JOIN relationship_types t ON t.id = g.relationship_type_id
         GROUP BY line ORDER BY min(g.depth), line`,
      );
      return r.rows.map((x) => x.line).join(' | ');
    },
  },
  {
    name: 'Guard: self-edge',
    run: async (c) => {
      const a = await newCi(c, 'application', 'self-edge-app');
      return expectReject(c, 'ci_relationships_no_self_edge', () => link(c, 'depends_on', a, a));
    },
  },
  {
    name: 'Guard: duplicate edge',
    run: async (c) => {
      const a = await newCi(c, 'application', 'dup-app');
      const d = await newCi(c, 'database', 'dup-db');
      await link(c, 'depends_on', a, d);
      return expectReject(c, 'ci_relationships_live_edge_uq', () => link(c, 'depends_on', a, d));
    },
  },
  {
    name: 'Guard: reverse duplicate of non-directional connected_to',
    run: async (c) => {
      const s = await newCi(c, 'server', 'conn-srv');
      const n = await newCi(c, 'network_device', 'conn-sw');
      await link(c, 'connected_to', s, n);
      return expectReject(c, 'ci_relationships_live_edge_uq', () => link(c, 'connected_to', n, s));
    },
  },
  {
    name: 'Guard: illegal endpoint classes (database located_in location)',
    run: async (c) => {
      const d = await newCi(c, 'database', 'rule-db');
      const l = await newCi(c, 'location', 'rule-loc');
      return expectReject(c, 'ci_relationships_endpoint_rule', () => link(c, 'located_in', d, l));
    },
  },
  {
    name: 'Guard: CI of an abstract class',
    run: async (c) => expectReject(c, 'configuration_items_class_concrete', () => newCi(c, 'hardware', 'abstract-ci')),
  },
  {
    name: 'Guard: attribute from another class',
    run: async (c) => {
      const a = await newCi(c, 'application', 'attr-app');
      const def = await one<{ id: string }>(
        c,
        `SELECT d.id FROM ci_attribute_definitions d JOIN ci_classes k ON k.id = d.class_id WHERE k.key = 'database' AND d.key = 'engine_version'`,
      );
      return expectReject(c, 'ci_attribute_values_attribute_in_class', () =>
        c.query(`INSERT INTO ci_attribute_values (ci_id, attribute_id, value_text) VALUES ($1, $2, '1.0')`, [a, def.id]),
      );
    },
  },
  {
    name: 'Guard: value stored in the wrong type column',
    run: async (c) => {
      const s = await newCi(c, 'server', 'type-srv');
      const def = await one<{ id: string }>(
        c,
        `SELECT d.id FROM ci_attribute_definitions d JOIN ci_classes k ON k.id = d.class_id WHERE k.key = 'server' AND d.key = 'cpu_cores'`,
      );
      return expectReject(c, 'ci_attribute_values_type_match', () =>
        c.query(`INSERT INTO ci_attribute_values (ci_id, attribute_id, value_text) VALUES ($1, $2, 'lots')`, [s, def.id]),
      );
    },
  },
  {
    name: 'Guard: enum value not in the allowed list',
    run: async (c) => {
      const s = await newCi(c, 'server', 'enum-srv');
      const def = await one<{ id: string }>(
        c,
        `SELECT d.id FROM ci_attribute_definitions d JOIN ci_classes k ON k.id = d.class_id WHERE k.key = 'server' AND d.key = 'os_family'`,
      );
      return expectReject(c, 'ci_attribute_values_enum', () =>
        c.query(`INSERT INTO ci_attribute_values (ci_id, attribute_id, value_text) VALUES ($1, $2, 'amiga')`, [s, def.id]),
      );
    },
  },
  {
    name: 'Guard: reference attribute pointing at the wrong class',
    run: async (c) => {
      const a = await newCi(c, 'application', 'ref-app');
      const s = await newCi(c, 'server', 'ref-srv');
      const def = await one<{ id: string }>(
        c,
        `SELECT d.id FROM ci_attribute_definitions d JOIN ci_classes k ON k.id = d.class_id WHERE k.key = 'application' AND d.key = 'primary_database'`,
      );
      return expectReject(c, 'ci_attribute_values_reference_class', () =>
        c.query(`INSERT INTO ci_attribute_values (ci_id, attribute_id, value_ref_ci_id) VALUES ($1, $2, $3)`, [a, def.id, s]),
      );
    },
  },
  {
    name: 'Guard: class hierarchy cycle',
    run: async (c) => {
      const hw = await id(c, 'ci_classes', 'hardware');
      const srv = await id(c, 'ci_classes', 'server');
      return expectReject(c, 'ci_classes_no_cycle', () => c.query(`UPDATE ci_classes SET parent_id = $1 WHERE id = $2`, [srv, hw]));
    },
  },
  {
    name: 'Guard: unknown status (foreign key)',
    run: async (c) =>
      expectReject(c, 'configuration_items_status_id_statuses_id_fk', () =>
        c.query(
          `INSERT INTO configuration_items (class_id, name, status_id) VALUES ((SELECT id FROM ci_classes WHERE key = 'server'), 'fk-srv', gen_random_uuid())`,
        ),
      ),
  },
  {
    name: 'Guard: audit_log is append-only',
    run: async (c) =>
      expectReject(c, 'audit_log is append-only', () => c.query(`UPDATE audit_log SET actor_name = 'tampered'`)),
  },
  {
    name: 'Soft delete: removed edge can be re-created; deleted CI cannot be linked',
    run: async (c) => {
      const a = await newCi(c, 'application', 'soft-app');
      const d = await newCi(c, 'database', 'soft-db');
      await link(c, 'depends_on', a, d);
      await c.query(`UPDATE ci_relationships SET deleted_at = now() WHERE source_ci_id = $1`, [a]);
      await link(c, 'depends_on', a, d);
      await c.query(`UPDATE configuration_items SET deleted_at = now() WHERE id = $1`, [d]);
      const s = await newCi(c, 'server', 'soft-srv');
      const res = await expectReject(c, 'ci_relationships_live_endpoints', () => link(c, 'runs_on', d, s));
      return `edge re-created after soft delete; linking deleted CI ${res}`;
    },
  },
  {
    name: 'Indexes used by the UI queries',
    run: async (c) => {
      await c.query('SET LOCAL enable_seqscan = off');
      const plan = async (q: string) =>
        (await c.query<{ 'QUERY PLAN': string }>(`EXPLAIN ${q}`)).rows.map((r) => r['QUERY PLAN']).join(' ');
      // Any of the listed indexes is acceptable (on tiny tables PG18 may prefer a skip scan of a wider index).
      const found = (p: string, ...idx: string[]) => {
        const hit = idx.find((i) => p.includes(i));
        if (!hit) throw new Error(`planner used none of ${idx.join(', ')}: ${p}`);
        return hit;
      };
      return [
        found(await plan(`SELECT id FROM configuration_items WHERE name ILIKE '%crm%'`), 'configuration_items_name_trgm_idx'),
        found(
          await plan(`SELECT id FROM configuration_items WHERE search_vector @@ plainto_tsquery('simple', 'crm')`),
          'configuration_items_search_idx',
        ),
        found(
          await plan(`SELECT id FROM configuration_items WHERE deleted_at IS NULL ORDER BY lower(name), id LIMIT 50`),
          'configuration_items_live_name_idx',
        ),
        found(
          await plan(`SELECT target_ci_id FROM ci_relationships WHERE source_ci_id = '00000000-0000-0000-0000-000000000000' AND deleted_at IS NULL`),
          'ci_relationships_source_idx',
          'ci_relationships_live_edge_uq',
        ),
        found(await plan(`SELECT id FROM configuration_items WHERE ip_address << '10.0.0.0/8'`), 'configuration_items_ip_idx'),
      ].join(', ');
    },
  },
];

async function main(): Promise<void> {
  const pool = createPool(loadEnv());
  const client = await pool.connect();
  let failed = 0;
  try {
    await client.query('BEGIN');
    for (const check of checks) {
      await client.query('SAVEPOINT chk');
      try {
        const detail = await check.run(client);
        await client.query('RELEASE SAVEPOINT chk');
        console.log(`PASS  ${check.name}\n      ${detail}`);
      } catch (err) {
        await client.query('ROLLBACK TO SAVEPOINT chk');
        failed++;
        console.log(`FAIL  ${check.name}\n      ${err instanceof Error ? err.message : String(err)}`);
      }
    }
  } finally {
    await client.query('ROLLBACK');
    client.release();
    await pool.end();
  }
  console.log(`\n${checks.length - failed}/${checks.length} checks passed (transaction rolled back, no data written)`);
  if (failed) process.exit(1);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch((err: unknown) => {
    console.error('Verify failed:', err instanceof Error ? err.message : err);
    process.exit(1);
  });
}
