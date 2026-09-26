import type { FastifyRequest } from 'fastify';
import type { AUDIT_ACTOR_TYPES } from '../db/schema/index.js';

/**
 * Who is making a change, as recorded in audit_log. This is the seam for
 * authentication: Milestone 1 has no auth, so the default resolver labels every
 * caller an unauthenticated api_client and takes an optional, untrusted display
 * name from the X-Actor-Name header. An auth module replaces the resolver (and
 * fills `id`) without touching routes or services.
 */
export interface Actor {
  type: (typeof AUDIT_ACTOR_TYPES)[number];
  id: string | null;
  name: string | null;
}

export type ActorResolver = (req: FastifyRequest) => Actor | Promise<Actor>;

export const anonymousActorResolver: ActorResolver = (req) => {
  const raw = req.headers['x-actor-name'];
  const name = typeof raw === 'string' ? raw.trim().slice(0, 200) : '';
  return { type: 'api_client', id: null, name: name || null };
};

/** Per-request context handed to services: the actor and a request id for audit correlation. */
export interface RequestContext {
  actor: Actor;
  requestId: string;
}
