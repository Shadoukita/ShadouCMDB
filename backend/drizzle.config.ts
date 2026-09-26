import { defineConfig } from 'drizzle-kit';

// Only used for `drizzle-kit generate` (schema diff -> SQL file), which needs no
// database connection. Migrations are applied by src/db/migrate.ts.
export default defineConfig({
  dialect: 'postgresql',
  schema: './src/db/schema/index.ts',
  out: '../sql/migrations',
  casing: 'snake_case',
});
