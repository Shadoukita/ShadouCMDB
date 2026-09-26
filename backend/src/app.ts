import { randomUUID } from 'node:crypto';
import Fastify, { type FastifyError, type FastifyInstance } from 'fastify';
import cors from '@fastify/cors';
import swagger from '@fastify/swagger';
import swaggerUi from '@fastify/swagger-ui';
import type pg from 'pg';
import type { Env } from './config/env.js';
import { createDb } from './db/client.js';
import { anonymousActorResolver, type ActorResolver } from './http/context.js';
import { AppError, isConnectionError, mapPgError } from './http/errors.js';
import { buildOpenApi } from './http/openapi.js';
import { registerRoutes, type RouteSpec } from './http/route.js';
import { auditRoutes } from './modules/audit.js';
import { classRoutes } from './modules/classes.js';
import { healthRoutes } from './modules/health.js';
import { itemRoutes } from './modules/items/routes.js';
import { lookupRoutes } from './modules/lookups.js';
import { relationshipRoutes } from './modules/relationships.js';

export const API_VERSION = '0.1.0';

export interface AppOptions {
  env: Env;
  pool: pg.Pool;
  /**
   * Seam for authentication: swap in a resolver that verifies a token and
   * returns the user. Route-level authorisation (RBAC) would hook in here too.
   */
  resolveActor?: ActorResolver;
}

/** Every route the API serves, in the order they appear in the OpenAPI document. */
export function buildRoutes(opts: Pick<AppOptions, 'pool'>): RouteSpec[] {
  const db = createDb(opts.pool);
  return [
    ...healthRoutes(opts.pool),
    ...itemRoutes(db),
    ...relationshipRoutes(db),
    ...classRoutes(db),
    ...lookupRoutes(db),
    ...auditRoutes(db),
  ];
}

const TAG_DESCRIPTIONS: Record<string, string> = {
  Health: 'Liveness and readiness probes for orchestrators and load balancers.',
  'Configuration items': 'CIs: the tracked assets. Includes the relationship graph around a CI.',
  Search: 'Global search across CIs and their attribute values.',
  Relationships: 'Typed, directional edges between CIs. Removal is a soft delete.',
  'CI classes': 'CI types in an inheritance tree. Adding a class is data entry, not a migration.',
  'Attribute definitions': 'Typed custom fields per class, inherited by subclasses.',
  'Relationship types': 'Relationship types (runs_on, depends_on, ...) and the rules for which classes they may connect.',
  Statuses: 'CI lifecycle statuses.',
  Environments: 'Deployment environments.',
  Locations: 'Location hierarchy (region > site > room > rack).',
  Owners: 'People and teams accountable for CIs.',
  'Audit log': 'Read-only change history written in the same transaction as every change.',
};

export function openApiDocument(routes: RouteSpec[]) {
  const info = {
    title: 'ShadouCMDB API',
    version: API_VERSION,
    description: [
      'REST API for ShadouCMDB. This API is the only database client; the web UI uses nothing else.',
      '',
      '- Collections are paginated with `limit`/`offset` and return `{ data, page: { limit, offset, total } }`.',
      '- `sort=field` ascending, `sort=-field` descending. `q` searches. Filters that take ids accept comma-separated lists.',
      '- Every error uses the `ErrorEnvelope` shape; invalid input is always 400 `VALIDATION_ERROR` with per-field `details`.',
      '- Writes are recorded in the audit log (`/api/v1/audit-log`). Send `X-Actor-Name` to label the actor until authentication exists.',
      '- Send `X-Request-Id` to correlate a request; it is echoed back and stored with audit rows.',
    ].join('\n'),
  };
  return buildOpenApi(routes, info, TAG_DESCRIPTIONS);
}

const REQUEST_ID = /^[A-Za-z0-9._:-]{1,128}$/;

export async function buildApp(opts: AppOptions): Promise<{ app: FastifyInstance; routes: RouteSpec[] }> {
  const { env } = opts;
  const app = Fastify({
    logger: { level: env.LOG_LEVEL },
    bodyLimit: 1024 * 1024,
    genReqId: (req) => {
      const h = req.headers['x-request-id'];
      return typeof h === 'string' && REQUEST_ID.test(h) ? h : randomUUID();
    },
  });

  app.addHook('onRequest', async (req, reply) => {
    reply.header('x-request-id', req.id);
  });

  // An empty body with Content-Type: application/json is treated as "no body"
  // (clients often send the header on DELETE); anything else uses the default,
  // prototype-poisoning-safe JSON parser.
  // JSON is the only accepted body type; anything else is 415.
  app.removeContentTypeParser('text/plain');
  const defaultJson = app.getDefaultJsonParser('error', 'error');
  app.removeContentTypeParser('application/json');
  app.addContentTypeParser('application/json', { parseAs: 'string' }, (req, body, done) => {
    if (body === '') return done(null, undefined);
    defaultJson(req, body as string, done);
  });

  if (env.CORS_ORIGINS.length > 0) {
    await app.register(cors, {
      origin: env.CORS_ORIGINS,
      methods: ['GET', 'POST', 'PATCH', 'DELETE'],
      exposedHeaders: ['x-request-id'],
    });
  }

  app.setErrorHandler((err: FastifyError, req, reply) => {
    let appErr = err instanceof AppError ? err : mapPgError(err);
    if (!appErr && isConnectionError(err)) {
      appErr = new AppError('DATABASE_UNAVAILABLE', 'The database is unreachable; try again shortly');
      req.log.error({ err }, 'database unavailable');
    }
    if (!appErr && err.statusCode && err.statusCode >= 400 && err.statusCode < 500) {
      // Fastify's own request errors (malformed JSON, wrong content type, body too large).
      if (err.code === 'FST_ERR_CTP_INVALID_MEDIA_TYPE') {
        appErr = new AppError('UNSUPPORTED_MEDIA_TYPE', 'Request bodies must be application/json');
      } else if (err.code === 'FST_ERR_CTP_BODY_TOO_LARGE') {
        appErr = new AppError('PAYLOAD_TOO_LARGE', 'Request body is too large');
      } else {
        appErr = AppError.field('(root)', err.message, err.code ?? 'bad_request');
      }
    }
    if (!appErr) {
      req.log.error({ err }, 'unhandled error');
      appErr = new AppError('INTERNAL_ERROR', 'An unexpected error occurred');
    }
    return reply.code(appErr.statusCode).send({
      error: {
        code: appErr.code,
        message: appErr.message,
        ...(appErr.details ? { details: appErr.details } : {}),
        requestId: String(req.id),
      },
    });
  });

  app.setNotFoundHandler((req, reply) =>
    reply.code(404).send({
      error: { code: 'NOT_FOUND', message: `Route ${req.method} ${req.url.split('?')[0]} does not exist`, requestId: String(req.id) },
    }),
  );

  const routes = buildRoutes(opts);
  registerRoutes(app, routes, {
    resolveActor: opts.resolveActor ?? anonymousActorResolver,
    checkResponses: env.NODE_ENV !== 'production',
  });

  const document = openApiDocument(routes);
  app.get('/openapi.json', async () => document);
  await app.register(swagger, { mode: 'static', specification: { document: document as never } });
  await app.register(swaggerUi, { routePrefix: '/docs' });

  return { app, routes };
}
