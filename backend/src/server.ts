import Fastify from 'fastify';
import { loadEnv } from './config/env.js';
import { createPool } from './db/client.js';
import { appliedMigrationCount, readJournal } from './db/migrate.js';

/*
 * Minimal service skeleton: liveness and readiness only. Resource routes
 * (CIs, classes, relationships, OpenAPI) arrive with the backend API task and
 * live in routes/ -> services/ -> data access layers.
 */
const env = loadEnv();
const pool = createPool(env);
const app = Fastify({ logger: { level: env.LOG_LEVEL } });

app.get('/healthz', async () => ({ status: 'ok' }));

app.get('/readyz', async (_req, reply) => {
  const expected = readJournal().entries.length;
  try {
    await pool.query('SELECT 1');
    const applied = await appliedMigrationCount(pool);
    const ready = applied === expected;
    return reply.code(ready ? 200 : 503).send({
      status: ready ? 'ready' : 'not_ready',
      database: 'ok',
      migrations: { applied, expected, upToDate: ready },
    });
  } catch (err) {
    app.log.warn({ err }, 'readiness check failed');
    return reply.code(503).send({ status: 'not_ready', database: 'unreachable', migrations: { expected } });
  }
});

const shutdown = async () => {
  await app.close();
  await pool.end();
  process.exit(0);
};
process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);

await app.listen({ host: env.API_HOST, port: env.API_PORT });
