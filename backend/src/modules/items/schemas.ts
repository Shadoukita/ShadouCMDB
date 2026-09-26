import { z } from 'zod';
import { ITEM_SORT_FIELDS } from '../../data/items.js';
import {
  Description,
  LookupRef,
  Name,
  OwnerRef,
  PageMeta,
  PageQuery,
  QueryUuidList,
  Timestamp,
  Uuid,
  component,
  listOf,
  sortParam,
} from '../../http/schemas.js';

export const AttributeValue = z
  .union([z.string(), z.number(), z.boolean()])
  .describe(
    'text/enum/date (YYYY-MM-DD)/datetime (ISO 8601)/ip/cidr/reference (CI id) are strings; number/integer are numbers; boolean is a boolean',
  );

export const ConfigurationItemSummary = component(
  'ConfigurationItemSummary',
  z.object({
    id: Uuid,
    name: z.string(),
    classId: Uuid,
    class: LookupRef,
    statusId: Uuid,
    status: LookupRef,
    environmentId: Uuid.nullable(),
    environment: LookupRef.nullable(),
    ownerId: Uuid.nullable(),
    owner: OwnerRef.nullable(),
    locationId: Uuid.nullable(),
    location: LookupRef.nullable(),
    hostname: z.string().nullable(),
    ipAddress: z.string().nullable(),
    serialNumber: z.string().nullable(),
    notes: z.string().nullable(),
    version: z.number().int().describe('Optimistic-locking counter; send it back in PATCH to detect concurrent edits'),
    createdAt: Timestamp,
    updatedAt: Timestamp,
    deletedAt: Timestamp.nullable().describe('Set when the CI was deleted (soft delete); history keeps resolving'),
  }),
);

export const ConfigurationItem = component(
  'ConfigurationItem',
  ConfigurationItemSummary.extend({
    attributes: z
      .record(z.string(), AttributeValue)
      .describe('Class attribute values by attribute key; unset attributes are absent'),
    attributeReferences: z
      .record(z.string(), z.object({ id: Uuid, name: z.string(), deleted: z.boolean() }))
      .describe('For reference attributes: the referenced CI, so the UI can show a name without another request'),
  }),
);

export const ConfigurationItemList = listOf('ConfigurationItemList', ConfigurationItemSummary);

const HOSTNAME = /^[A-Za-z0-9]([A-Za-z0-9._-]{0,252})$/;
const IpAddress = z.union([z.ipv4(), z.ipv6()], { error: 'Must be an IPv4 or IPv6 address' });
const Cidr = z.union([z.cidrv4(), z.cidrv6()], { error: 'Must be a CIDR block, e.g. 10.0.0.0/24' });

const itemWritable = {
  name: Name,
  statusId: Uuid,
  environmentId: Uuid.nullable().optional(),
  ownerId: Uuid.nullable().optional(),
  locationId: Uuid.nullable().optional(),
  hostname: z.string().regex(HOSTNAME, 'Letters, digits, ".", "_" and "-", starting with a letter or digit').nullable().optional(),
  ipAddress: IpAddress.nullable().optional().describe('IPv4 or IPv6 address'),
  serialNumber: z.string().trim().min(1).max(200).nullable().optional(),
  notes: Description.nullable().optional(),
};

export const CreateItemBody = z.strictObject({
  classId: Uuid.describe('A concrete (non-abstract), active class'),
  ...itemWritable,
  attributes: z
    .record(z.string(), AttributeValue.nullable())
    .optional()
    .describe('Values by attribute key (see GET /api/v1/ci-classes/{id}/attributes). Required attributes must be present.'),
});

export const UpdateItemBody = z
  .strictObject({
    classId: Uuid.optional().describe('Changing class requires clearing attributes the new class does not have'),
    ...itemWritable,
    name: Name.optional(),
    statusId: Uuid.optional(),
    attributes: z
      .record(z.string(), AttributeValue.nullable())
      .optional()
      .describe('Merged into the current values; null clears an attribute'),
    version: z.number().int().min(1).optional().describe('If sent and stale, the update fails with 409 VERSION_CONFLICT'),
  })
  .refine((o) => Object.keys(o).some((k) => k !== 'version'), { message: 'Provide at least one field to update' });

const deletedMode = z
  .enum(['exclude', 'include', 'only'])
  .default('exclude')
  .describe('Soft-deleted CIs: exclude (default), include, or only');

const itemFilterShape = {
  classId: QueryUuidList.optional().describe('Filter by class (includes subclasses unless includeSubclasses=false)'),
  includeSubclasses: z.enum(['true', 'false']).default('true').transform((v) => v === 'true'),
  statusId: QueryUuidList.optional(),
  environmentId: QueryUuidList.optional(),
  ownerId: QueryUuidList.optional(),
  locationId: QueryUuidList.optional(),
  ipWithin: Cidr.optional().describe('Only CIs whose ipAddress is inside this CIDR, e.g. 10.20.0.0/16'),
  deleted: deletedMode,
};

export const ListItemsQuery = z.strictObject({
  ...PageQuery,
  q: z
    .string()
    .trim()
    .min(1)
    .max(200)
    .optional()
    .describe('Search name, hostname, serial number, IP address, notes and attribute values'),
  sort: sortParam(ITEM_SORT_FIELDS, 'name'),
  ...itemFilterShape,
});

export const SearchQuery = z.strictObject({
  ...PageQuery,
  q: z.string().trim().min(1).max(200).describe('Search text'),
  ...itemFilterShape,
});

export const SearchMatch = z.object({
  field: z.string().describe('"name", "hostname", "serialNumber", "ipAddress", "notes" or "attributes.<key>"'),
  label: z.string(),
  value: z.string(),
});

export const SearchResults = component(
  'SearchResults',
  z.object({
    data: z.array(z.object({ item: ConfigurationItemSummary, matches: z.array(SearchMatch) })),
    page: PageMeta,
  }),
);

// ---------------------------------------------------------------------------
// Graph
// ---------------------------------------------------------------------------

export const GraphQuery = z.strictObject({
  depth: z.coerce.number().int().min(1).max(6).default(2).describe('Hops from the root CI (1-6)'),
  direction: z
    .enum(['both', 'outgoing', 'incoming'])
    .default('both')
    .describe('outgoing follows source->target (app -> runs_on -> server); incoming the reverse; symmetric types are always followed'),
  relationshipTypeId: QueryUuidList.optional().describe('Only follow these relationship types'),
  maxNodes: z.coerce.number().int().min(1).max(1000).default(250).describe('Stop expanding once this many CIs are collected'),
});

export const GraphEdge = component(
  'GraphEdge',
  z.object({
    id: Uuid.describe('Relationship id'),
    relationshipTypeId: Uuid,
    type: z.object({
      key: z.string(),
      name: z.string(),
      forwardLabel: z.string(),
      reverseLabel: z.string(),
      isDirectional: z.boolean(),
    }),
    sourceCiId: Uuid,
    targetCiId: Uuid,
    notes: z.string().nullable(),
  }),
);

export const Graph = component(
  'RelationshipGraph',
  z.object({
    rootId: Uuid,
    depth: z.number().int(),
    direction: z.enum(['both', 'outgoing', 'incoming']),
    nodes: z.array(ConfigurationItemSummary.extend({ depth: z.number().int().describe('Hops from the root (root = 0)') })),
    edges: z.array(GraphEdge),
    truncated: z.boolean().describe('true when maxNodes stopped the expansion early'),
  }),
);
