import { buildApp } from './app.js';
import { loadEnv } from './config/env.js';
import { createPool } from './db/client.js';

const env = loadEnv();
const pool = createPool(env);
// Idle clients can lose their connection (DB restart, failover); log instead of crashing.
pool.on('error', (err) => console.error('idle database client error:', err.message));

const { app } = await buildApp({ env, pool });

const shutdown = async () => {
  await app.close();
  await pool.end();
  process.exit(0);
};
process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);

await app.listen({ host: env.API_HOST, port: env.API_PORT });
