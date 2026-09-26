/*
 * Writes backend/openapi.json from the route table (no database needed), so the
 * frontend can generate a typed client from a file in the repo.
 *
 *   npm run openapi -w backend            # regenerate
 *   npm run openapi -w backend -- --check # fail if the committed file is stale (CI)
 */
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import pg from 'pg';
import { buildRoutes, openApiDocument } from '../src/app.js';

const target = join(dirname(fileURLToPath(import.meta.url)), '..', 'openapi.json');
// A pool that is never used: building routes does not connect.
const pool = new pg.Pool({ host: 'unused.invalid' });
const doc = `${JSON.stringify(openApiDocument(buildRoutes({ pool })), null, 2)}\n`;
await pool.end();

if (process.argv.includes('--check')) {
  let current = '';
  try {
    current = readFileSync(target, 'utf8');
  } catch {
    // missing file counts as stale
  }
  if (current !== doc) {
    console.error('backend/openapi.json is out of date. Run: npm run openapi -w backend');
    process.exit(1);
  }
  console.log('backend/openapi.json is up to date');
} else {
  writeFileSync(target, doc);
  const ops = Object.values(JSON.parse(doc).paths as Record<string, object>).reduce((n, p) => n + Object.keys(p).length, 0);
  console.log(`Wrote ${target} (${ops} operations)`);
}
