import type { Database } from '../../db/client.js';
import { defineRoute, type RouteSpec } from '../../http/route.js';
import { IdParams } from '../../http/schemas.js';
import {
  ConfigurationItem,
  ConfigurationItemList,
  CreateItemBody,
  Graph,
  GraphQuery,
  ListItemsQuery,
  SearchQuery,
  SearchResults,
  UpdateItemBody,
} from './schemas.js';
import { ItemService } from './service.js';

const TAG = 'Configuration items';
const BASE = '/api/v1/configuration-items';

export function itemRoutes(db: Database): RouteSpec[] {
  const items = new ItemService(db);
  return [
    defineRoute({
      method: 'GET',
      url: BASE,
      operationId: 'listConfigurationItems',
      tag: TAG,
      summary: 'Inventory list: paginated, searchable, filterable, sortable',
      description: 'Returns summaries (no attribute values). Soft-deleted CIs are hidden unless `deleted=include|only`.',
      query: ListItemsQuery,
      response: ConfigurationItemList,
      handler: async ({ query }) => items.list(query),
    }),
    defineRoute({
      method: 'GET',
      url: `${BASE}/:id`,
      operationId: 'getConfigurationItem',
      tag: TAG,
      summary: 'Get a CI with its attribute values',
      description: 'Deleted CIs are still returned (with `deletedAt` set) so history and old links resolve.',
      params: IdParams,
      response: ConfigurationItem,
      errors: ['NOT_FOUND'],
      handler: async ({ params }) => items.get(params.id),
    }),
    defineRoute({
      method: 'POST',
      url: BASE,
      operationId: 'createConfigurationItem',
      tag: TAG,
      summary: 'Create a CI, including its attribute values',
      body: CreateItemBody,
      status: 201,
      response: ConfigurationItem,
      handler: async ({ body, actor, requestId }) => items.create({ actor, requestId }, body),
    }),
    defineRoute({
      method: 'PATCH',
      url: `${BASE}/:id`,
      operationId: 'updateConfigurationItem',
      tag: TAG,
      summary: 'Update a CI (partial); attributes are merged, null clears one',
      params: IdParams,
      body: UpdateItemBody,
      response: ConfigurationItem,
      errors: ['NOT_FOUND', 'CONFLICT', 'VERSION_CONFLICT'],
      handler: async ({ params, body, actor, requestId }) => items.update({ actor, requestId }, params.id, body),
    }),
    defineRoute({
      method: 'DELETE',
      url: `${BASE}/:id`,
      operationId: 'deleteConfigurationItem',
      tag: TAG,
      summary: 'Delete a CI (soft delete)',
      description: 'Sets `deletedAt` on the CI and soft-deletes its live relationships in the same transaction. Both stay readable for history.',
      params: IdParams,
      errors: ['NOT_FOUND'],
      handler: async ({ params, actor, requestId }) => items.remove({ actor, requestId }, params.id),
    }),
    defineRoute({
      method: 'GET',
      url: `${BASE}/:id/graph`,
      operationId: 'getConfigurationItemGraph',
      tag: TAG,
      summary: 'Relationship graph around a CI in one call (nodes + edges)',
      description:
        'Breadth-first traversal of live relationships up to `depth` hops. For a Server -> Application -> Database view, ask from the application with `direction=outgoing`, or from the server with `direction=both&depth=2`.',
      params: IdParams,
      query: GraphQuery,
      response: Graph,
      errors: ['NOT_FOUND'],
      handler: async ({ params, query }) => items.graph(params.id, query),
    }),
    defineRoute({
      method: 'GET',
      url: '/api/v1/search',
      operationId: 'searchConfigurationItems',
      tag: 'Search',
      summary: 'Global search across CIs, ranked, with the fields that matched',
      description:
        'Matches name, hostname and serial number (substring), IP address (prefix, or containment when `q` is an IP or CIDR), notes (word prefix) and attribute values (text/enum substring, IP/CIDR prefix). Exact matches rank first, then name prefix, then trigram similarity.',
      query: SearchQuery,
      response: SearchResults,
      handler: async ({ query }) => items.search(query),
    }),
  ];
}
