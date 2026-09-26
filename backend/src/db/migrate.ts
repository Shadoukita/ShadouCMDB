import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import type pg from 'pg';
import { drizzle } from 'drizzle-orm/node-postgres';
import { migrate } from 'drizzle-orm/node-postgres/migrator';
import { loadEnv } from '../config/env.js';
import { createPool } from './client.js';

// Migrations live in the repo-level sql/migrations folder. This file sits at the
// same depth in src/ and dist/ (backend/{src,dist}/db/), so one relative path works
// for both tsx and the compiled build, including the Docker image.
export const MIGRATIONS_FOLDER = resolve(dirname(fileURLToPath(import.meta.url)), '../../../sql/migrations');
const MIGRATIONS_SCHEMA = 'drizzle';
const MIGRATIONS_TABLE = '__drizzle_migrations';

interface Journal {
  entries: { idx: number; tag: string; when: number }[];
}

export function readJournal(): Journal {
  return JSON.parse(readFileSync(join(MIGRATIONS_FOLDER, 'meta', '_journal.json'), 'utf8')) as Journal;
}

/** True when the Rust binary (`shadoucmdb migrate`) manages this database's migrations. */
async function managedByRust(pool: pg.Pool): Promise<boolean> {
  const res = await pool.query<{ t: string | null }>(`SELECT to_regclass('_sqlx_migrations') AS t`);
  return Boolean(res.rows[0]?.t);
}

/** Number of migrations recorded as applied (0 on an empty database). */
export async function appliedMigrationCount(pool: pg.Pool): Promise<number> {
  // Until the Node API is retired (SHAA-9), accept databases migrated by the Rust binary too.
  if (await managedByRust(pool)) {
    const res = await pool.query<{ n: string }>(`SELECT count(*) AS n FROM _sqlx_migrations WHERE success`);
    return Number(res.rows[0]?.n ?? 0);
  }
  const exists = await pool.query<{ t: string | null }>(`SELECT to_regclass($1) AS t`, [
    `${MIGRATIONS_SCHEMA}.${MIGRATIONS_TABLE}`,
  ]);
  if (!exists.rows[0]?.t) return 0;
  const res = await pool.query<{ n: string }>(
    `SELECT count(*) AS n FROM "${MIGRATIONS_SCHEMA}"."${MIGRATIONS_TABLE}"`,
  );
  return Number(res.rows[0]?.n ?? 0);
}

async function main(): Promise<void> {
  const env = loadEnv();
  const pool = createPool(env);
  try {
    const server = await pool.query<{ db: string; version: string }>(
      `SELECT current_database() AS db, current_setting('server_version') AS version`,
    );
    const info = server.rows[0];
    console.log(`Connected to database "${info?.db}" (PostgreSQL ${info?.version}), ssl=${env.DATABASE_SSL}`);

    const journal = readJournal();
    if (await managedByRust(pool)) {
      const applied = await appliedMigrationCount(pool);
      console.log(`Database is at migration ${applied}/${journal.entries.length} (managed by \`shadoucmdb migrate\`)`);
      if (applied < journal.entries.length) {
        throw new Error('pending migrations: apply them with `shadoucmdb migrate` (see README)');
      }
      return;
    }
    const before = await appliedMigrationCount(pool);
    const pending = journal.entries.slice(before);
    console.log(`Migrations: ${journal.entries.length} in repo, ${before} applied, ${pending.length} pending`);

    // Drizzle runs all pending migrations in a single transaction and records
    // each one in drizzle.__drizzle_migrations; re-running is a no-op.
    await migrate(drizzle(pool), {
      migrationsFolder: MIGRATIONS_FOLDER,
      migrationsSchema: MIGRATIONS_SCHEMA,
      migrationsTable: MIGRATIONS_TABLE,
    });

    for (const m of pending) console.log(`  applied ${m.tag}`);
    const after = await appliedMigrationCount(pool);
    console.log(`Database is at migration ${after}/${journal.entries.length}${pending.length ? '' : ' (nothing to do)'}`);
  } finally {
    await pool.end();
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch((err: unknown) => {
    console.error('Migration failed:', err instanceof Error ? err.message : err);
    process.exit(1);
  });
}
