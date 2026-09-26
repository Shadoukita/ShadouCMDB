import { readFileSync } from 'node:fs';
import { z } from 'zod';
import type { PoolConfig } from 'pg';

/**
 * All runtime configuration comes from environment variables. There are no
 * host/port/user defaults: PostgreSQL is an external service and the operator
 * must say where it lives. See .env.example for the full list.
 */
const EnvSchema = z
  .object({
    NODE_ENV: z.enum(['development', 'production', 'test']).default('production'),
    LOG_LEVEL: z.enum(['fatal', 'error', 'warn', 'info', 'debug', 'trace', 'silent']).default('info'),
    API_HOST: z.string().min(1).default('0.0.0.0'),
    API_PORT: z.coerce.number().int().min(1).max(65535).default(3000),

    // Either a single connection string...
    DATABASE_URL: z.string().min(1).optional(),
    // ...or discrete libpq-style variables.
    PGHOST: z.string().min(1).optional(),
    PGPORT: z.coerce.number().int().min(1).max(65535).optional(),
    PGDATABASE: z.string().min(1).optional(),
    PGUSER: z.string().min(1).optional(),
    PGPASSWORD: z.string().optional(),

    DATABASE_SSL: z.enum(['disable', 'require', 'verify-full']).default('require'),
    DATABASE_SSL_CA_FILE: z.string().min(1).optional(),
    DATABASE_POOL_MAX: z.coerce.number().int().min(1).max(200).default(10),
    DATABASE_STATEMENT_TIMEOUT_MS: z.coerce.number().int().min(0).default(30_000),
  })
  .superRefine((env, ctx) => {
    if (env.DATABASE_URL) return;
    for (const key of ['PGHOST', 'PGDATABASE', 'PGUSER'] as const) {
      if (!env[key]) {
        ctx.addIssue({
          code: 'custom',
          path: [key],
          message: `${key} is required when DATABASE_URL is not set`,
        });
      }
    }
  });

export type Env = z.infer<typeof EnvSchema>;

export function loadEnv(source: NodeJS.ProcessEnv = process.env): Env {
  // Treat empty strings (common in .env files and compose) as unset.
  const cleaned = Object.fromEntries(Object.entries(source).filter(([, v]) => v !== ''));
  const parsed = EnvSchema.safeParse(cleaned);
  if (!parsed.success) {
    const detail = parsed.error.issues.map((i) => `  - ${i.path.join('.')}: ${i.message}`).join('\n');
    throw new Error(`Invalid configuration:\n${detail}\nSee .env.example for every supported variable.`);
  }
  return parsed.data;
}

export function buildPoolConfig(env: Env): PoolConfig {
  let ssl: PoolConfig['ssl'];
  if (env.DATABASE_SSL === 'disable') {
    ssl = false;
  } else {
    ssl = {
      rejectUnauthorized: env.DATABASE_SSL === 'verify-full',
      ...(env.DATABASE_SSL_CA_FILE ? { ca: readFileSync(env.DATABASE_SSL_CA_FILE, 'utf8') } : {}),
    };
  }

  const base: PoolConfig = {
    ssl,
    max: env.DATABASE_POOL_MAX,
    statement_timeout: env.DATABASE_STATEMENT_TIMEOUT_MS || undefined,
    application_name: 'shadoucmdb',
  };

  if (env.DATABASE_URL) {
    // Strip sslmode from the URL so DATABASE_SSL is the single source of truth
    // (pg would otherwise let the URL override the ssl object).
    const url = new URL(env.DATABASE_URL);
    url.searchParams.delete('sslmode');
    return { ...base, connectionString: url.toString() };
  }

  return {
    ...base,
    host: env.PGHOST,
    port: env.PGPORT ?? 5432,
    database: env.PGDATABASE,
    user: env.PGUSER,
    password: env.PGPASSWORD,
  };
}
