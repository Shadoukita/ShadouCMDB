import { z } from 'zod';
import type { ErrorCode } from './errors.js';
import type { RouteSpec } from './route.js';
import { components } from './schemas.js';

type JsonSchema = Record<string, unknown>;

const REF_PREFIX = '#/components/schemas/';

const ERROR_STATUS: Record<ErrorCode, { status: string; description: string }> = {
  VALIDATION_ERROR: { status: '400', description: 'Invalid input (code VALIDATION_ERROR) with per-field details' },
  NOT_FOUND: { status: '404', description: 'Not found (code NOT_FOUND)' },
  CONFLICT: { status: '409', description: 'Conflict: CONFLICT (duplicate), IN_USE or VERSION_CONFLICT' },
  IN_USE: { status: '409', description: 'Conflict: CONFLICT (duplicate), IN_USE or VERSION_CONFLICT' },
  VERSION_CONFLICT: { status: '409', description: 'Conflict: CONFLICT (duplicate), IN_USE or VERSION_CONFLICT' },
  UNSUPPORTED_MEDIA_TYPE: { status: '415', description: 'Body is not application/json' },
  PAYLOAD_TOO_LARGE: { status: '413', description: 'Body too large' },
  DATABASE_UNAVAILABLE: { status: '503', description: 'Database unreachable (code DATABASE_UNAVAILABLE)' },
  INTERNAL_ERROR: { status: '500', description: 'Unexpected server error (code INTERNAL_ERROR)' },
};

function strip(schema: JsonSchema): JsonSchema {
  const { $schema: _s, $id: _i, ...rest } = schema;
  return rest;
}

/** Inline (request-side) JSON schema for a zod schema. */
function inputSchema(schema: z.ZodType): JsonSchema {
  return strip(z.toJSONSchema(schema, { io: 'input', unrepresentable: 'any' }) as JsonSchema);
}

function parameters(schema: z.ZodType | undefined, where: 'path' | 'query'): JsonSchema[] {
  if (!schema) return [];
  const json = inputSchema(schema);
  const props = (json.properties ?? {}) as Record<string, JsonSchema>;
  const required = new Set((json.required as string[] | undefined) ?? []);
  return Object.entries(props).map(([name, prop]) => {
    const { description, ...propSchema } = prop;
    return {
      name,
      in: where,
      required: where === 'path' || (required.has(name) && !('default' in propSchema)),
      ...(description ? { description } : {}),
      schema: propSchema,
    };
  });
}

/**
 * Build the OpenAPI 3.1 document from the route table. Response schemas go
 * through one registry conversion so shared components become $refs.
 */
export function buildOpenApi(
  routes: RouteSpec[],
  info: { title: string; version: string; description: string },
  tagDescriptions: Record<string, string> = {},
) {
  // Registry of every response schema: the shared components plus one entry per
  // route whose response schema is not already a named component.
  const responseRegistry = z.registry<{ id: string }>();
  const idOf = new Map<z.ZodType, string>();
  for (const { id, schema } of components) {
    responseRegistry.add(schema, { id });
    idOf.set(schema, id);
  }
  for (const r of routes) {
    if (r.response && !idOf.has(r.response)) {
      const id = `${r.operationId[0]!.toUpperCase()}${r.operationId.slice(1)}Response`;
      responseRegistry.add(r.response, { id });
      idOf.set(r.response, id);
    }
  }
  const converted = z.toJSONSchema(responseRegistry, {
    io: 'output',
    unrepresentable: 'any',
    uri: (id) => `${REF_PREFIX}${id}`,
  }) as { schemas: Record<string, JsonSchema> };
  const schemas = Object.fromEntries(
    Object.entries(converted.schemas)
      .filter(([id]) => id !== '__shared')
      .map(([id, s]) => [id, strip(s)]),
  );

  const paths: Record<string, Record<string, unknown>> = {};
  for (const r of routes) {
    const path = r.url.replace(/:([A-Za-z0-9_]+)/g, '{$1}');
    const status = String(r.status ?? (r.response ? 200 : 204));
    const responses: Record<string, unknown> = {
      [status]: r.response
        ? { description: 'Success', content: { 'application/json': { schema: { $ref: `${REF_PREFIX}${idOf.get(r.response)}` } } } }
        : { description: 'Success, no content' },
    };
    for (const alt of r.alsoReturns ?? []) {
      responses[String(alt.status)] = {
        description: alt.description,
        content: { 'application/json': { schema: { $ref: `${REF_PREFIX}${idOf.get(r.response!)}` } } },
      };
    }
    const errorCodes = new Set<ErrorCode>([
      ...(r.params || r.query || r.body ? (['VALIDATION_ERROR'] as const) : []),
      ...(r.errors ?? []),
      'INTERNAL_ERROR',
    ]);
    if (!r.url.startsWith('/healthz') && !r.url.startsWith('/readyz') && !r.url.startsWith('/openapi')) {
      errorCodes.add('DATABASE_UNAVAILABLE');
    }
    if (r.body) errorCodes.add('UNSUPPORTED_MEDIA_TYPE');
    for (const code of errorCodes) {
      const e = ERROR_STATUS[code];
      if (responses[e.status]) continue;
      responses[e.status] = {
        description: e.description,
        content: { 'application/json': { schema: { $ref: `${REF_PREFIX}ErrorEnvelope` } } },
      };
    }

    paths[path] ??= {};
    paths[path][r.method.toLowerCase()] = {
      operationId: r.operationId,
      tags: [r.tag],
      summary: r.summary,
      ...(r.description ? { description: r.description } : {}),
      parameters: [...parameters(r.params, 'path'), ...parameters(r.query, 'query'), ...actorHeader(r)],
      ...(r.body
        ? { requestBody: { required: true, content: { 'application/json': { schema: inputSchema(r.body) } } } }
        : {}),
      responses,
    };
  }

  return {
    openapi: '3.1.0',
    info,
    servers: [{ url: '/', description: 'Same origin the document was fetched from' }],
    // Milestone 1 has no authentication. An auth module adds securitySchemes here.
    security: [],
    tags: [...new Set(routes.map((r) => r.tag))].map((name) => ({
      name,
      ...(tagDescriptions[name] ? { description: tagDescriptions[name] } : {}),
    })),
    paths,
    components: { schemas },
  };
}

function actorHeader(r: RouteSpec): JsonSchema[] {
  if (r.method === 'GET') return [];
  return [
    {
      name: 'X-Actor-Name',
      in: 'header',
      required: false,
      description:
        'Optional display name recorded as the actor in audit_log. Unauthenticated in Milestone 1; replaced by the authenticated user once auth exists.',
      schema: { type: 'string', maxLength: 200 },
    },
  ];
}
