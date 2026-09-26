import pg from 'pg';
import { drizzle, type NodePgDatabase } from 'drizzle-orm/node-postgres';
import { buildPoolConfig, type Env } from '../config/env.js';
import * as schema from './schema/index.js';

export type Database = NodePgDatabase<typeof schema>;

export function createPool(env: Env): pg.Pool {
  return new pg.Pool(buildPoolConfig(env));
}

export function createDb(pool: pg.Pool): Database {
  return drizzle(pool, { schema });
}
