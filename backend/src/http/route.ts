import type { FastifyInstance, FastifyReply, FastifyRequest } from 'fastify';
import { z } from 'zod';
import { AppError, zodToFieldErrors, type ErrorCode } from './errors.js';
import type { ActorResolver, RequestContext } from './context.js';

type AnyObject = z.ZodType<Record<string, unknown>>;

/**
 * A route is declared once, with zod schemas for its params, query, body and
 * response. The same declaration drives request validation, the OpenAPI
 * document (http/openapi.ts) and, outside production, a response check, so
 * the spec cannot drift from the code.
 */
export interface RouteSpec<
  P extends AnyObject | undefined = AnyObject | undefined,
  Q extends AnyObject | undefined = AnyObject | undefined,
  B extends z.ZodType | undefined = z.ZodType | undefined,
  R extends z.ZodType | undefined = z.ZodType | undefined,
> {
  method: 'GET' | 'POST' | 'PATCH' | 'DELETE';
  /** Fastify path syntax, e.g. /api/v1/statuses/:id */
  url: string;
  operationId: string;
  tag: string;
  summary: string;
  description?: string;
  params?: P;
  query?: Q;
  body?: B;
  /** Success status; defaults to 200, or 204 when there is no response schema. */
  status?: number;
  response?: R;
  /** Error codes this route can return, beyond VALIDATION_ERROR / INTERNAL_ERROR / DATABASE_UNAVAILABLE. */
  errors?: ErrorCode[];
  /** Other statuses that return the success schema (e.g. /readyz answers 503 with the same body). */
  alsoReturns?: { status: number; description: string }[];
  handler: (ctx: HandlerContext<P, Q, B>) => Promise<R extends z.ZodType ? z.output<R> : void>;
}

export interface HandlerContext<P, Q, B> extends RequestContext {
  params: P extends z.ZodType ? z.output<P> : Record<string, never>;
  query: Q extends z.ZodType ? z.output<Q> : Record<string, never>;
  body: B extends z.ZodType ? z.output<B> : undefined;
  req: FastifyRequest;
  reply: FastifyReply;
}

/** Identity helper that keeps the generic parameters inferred at the call site. */
export function defineRoute<
  P extends AnyObject | undefined = undefined,
  Q extends AnyObject | undefined = undefined,
  B extends z.ZodType | undefined = undefined,
  R extends z.ZodType | undefined = undefined,
>(spec: RouteSpec<P, Q, B, R>): RouteSpec {
  return spec as unknown as RouteSpec;
}

/** Repeated query keys (?a=1&a=2) are treated like a comma-separated list. */
function normaliseQuery(raw: unknown): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries((raw ?? {}) as Record<string, unknown>)) {
    out[k] = Array.isArray(v) ? v.join(',') : v;
  }
  return out;
}

function parse<T extends z.ZodType>(schema: T, input: unknown, where: 'body' | 'query' | 'params'): z.output<T> {
  const res = schema.safeParse(input);
  if (!res.success) throw AppError.validation(zodToFieldErrors(res.error, where));
  return res.data;
}

export interface RegisterOptions {
  resolveActor: ActorResolver;
  /** Check every response against its declared schema (on in development and test). */
  checkResponses: boolean;
}

export function registerRoutes(app: FastifyInstance, routes: RouteSpec[], opts: RegisterOptions): void {
  for (const route of routes) {
    const successStatus = route.status ?? (route.response ? 200 : 204);
    app.route({
      method: route.method,
      url: route.url,
      handler: async (req, reply) => {
        const params = route.params ? parse(route.params, req.params, 'params') : {};
        const query = route.query ? parse(route.query, normaliseQuery(req.query), 'query') : {};
        let body: unknown;
        if (route.body) {
          body = parse(route.body, req.body, 'body');
        }
        const actor = await opts.resolveActor(req);
        const result = await route.handler({
          params,
          query,
          body,
          req,
          reply,
          actor,
          requestId: String(req.id),
        } as never);

        // A handler may pick a different status itself (e.g. /readyz -> 503).
        if (reply.statusCode === 200) reply.code(successStatus);
        if (!route.response) return reply.send();
        if (opts.checkResponses) {
          const check = route.response.safeParse(JSON.parse(JSON.stringify(result)));
          if (!check.success) {
            req.log.error({ issues: check.error.issues, operationId: route.operationId }, 'response does not match OpenAPI schema');
            throw new Error(`Response for ${route.operationId} does not match its declared schema`);
          }
        }
        return reply.send(result);
      },
    });
  }
}
