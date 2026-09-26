import type pg from 'pg';
import { z } from 'zod';
import { appliedMigrationCount, readJournal } from '../db/migrate.js';
import { defineRoute, type RouteSpec } from '../http/route.js';
import { component } from '../http/schemas.js';

const Liveness = component('Liveness', z.object({ status: z.literal('ok') }));

const Readiness = component(
  'Readiness',
  z.object({
    status: z.enum(['ready', 'not_ready']),
    database: z.enum(['ok', 'unreachable']),
    migrations: z.object({
      applied: z.number().int().optional().describe('Absent when the database is unreachable'),
      expected: z.number().int().describe('Migrations shipped with this build'),
      upToDate: z.boolean().optional(),
    }),
  }),
);

export function healthRoutes(pool: pg.Pool): RouteSpec[] {
  return [
    defineRoute({
      method: 'GET',
      url: '/healthz',
      operationId: 'getLiveness',
      tag: 'Health',
      summary: 'Liveness: the process is up (does not touch the database)',
      response: Liveness,
      handler: async () => ({ status: 'ok' as const }),
    }),
    defineRoute({
      method: 'GET',
      url: '/readyz',
      operationId: 'getReadiness',
      tag: 'Health',
      summary: 'Readiness: database reachable and all migrations applied',
      description:
        'Returns 200 with status "ready" only when the database answers and every migration in this build is applied; otherwise 503 with the same body shape.',
      response: Readiness,
      alsoReturns: [{ status: 503, description: 'Not ready: database unreachable or migrations pending' }],
      handler: async ({ reply, req }) => {
        const expected = readJournal().entries.length;
        try {
          await pool.query('SELECT 1');
          const applied = await appliedMigrationCount(pool);
          const ready = applied === expected;
          reply.code(ready ? 200 : 503);
          return { status: ready ? 'ready' : 'not_ready', database: 'ok', migrations: { applied, expected, upToDate: ready } } as const;
        } catch (err) {
          req.log.warn({ err }, 'readiness check failed');
          reply.code(503);
          return { status: 'not_ready', database: 'unreachable', migrations: { expected } } as const;
        }
      },
    }),
  ];
}
