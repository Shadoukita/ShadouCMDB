import { eq, inArray, isNull } from 'drizzle-orm';
import { z } from 'zod';
import type { Database } from '../db/client.js';
import { LOCATION_TYPES, OWNER_KINDS, environments, locations, owners, statuses } from '../db/schema/index.js';
import type { RouteSpec } from '../http/route.js';
import { Description, Key, Name, QueryBool, QueryUuidList, Timestamp, Uuid, component } from '../http/schemas.js';
import {
  SimpleResourceService,
  activeFilter,
  nonEmptyPatch,
  simpleResourceRoutes,
  type SimpleResourceConfig,
} from './simple-resource.js';

/*
 * Lookup tables: statuses, environments, locations, owners. Rows are renamed
 * and retired (isActive=false) rather than deleted once CIs reference them.
 */

const lookupDto = {
  id: Uuid,
  key: z.string(),
  name: z.string(),
  description: z.string().nullable(),
  sortOrder: z.number().int(),
  isActive: z.boolean(),
  createdAt: Timestamp,
  updatedAt: Timestamp,
};

const lookupWritable = {
  key: Key,
  name: Name,
  description: Description.nullable().optional(),
  sortOrder: z.number().int().min(-1_000_000).max(1_000_000).optional(),
  isActive: z.boolean().optional(),
};

const lookupSortFields = ['sortOrder', 'name', 'key', 'createdAt', 'updatedAt'] as const;

// ---------------------------------------------------------------------------

const Status = component(
  'Status',
  z.object({ ...lookupDto, isOperational: z.boolean().describe('Counts as "live" in reports (in_service, maintenance)') }),
);
const statusWritable = { ...lookupWritable, isOperational: z.boolean().optional() };

const statusConfig: SimpleResourceConfig<typeof lookupSortFields> = {
  table: statuses,
  entityType: 'statuses',
  label: 'Status',
  basePath: '/api/v1/statuses',
  tag: 'Statuses',
  names: { singular: 'status', plural: 'statuses' },
  dto: Status,
  createBody: z.strictObject(statusWritable),
  updateBody: nonEmptyPatch(z.strictObject(statusWritable).partial()),
  filterShape: { isActive: QueryBool.optional(), isOperational: QueryBool.optional() },
  filters: (q) => [activeFilter(statuses.isActive)(q.isActive), activeFilter(statuses.isOperational)(q.isOperational)],
  searchColumns: [statuses.key, statuses.name, statuses.description],
  sortFields: lookupSortFields,
  sortColumns: {
    sortOrder: statuses.sortOrder,
    name: statuses.name,
    key: statuses.key,
    createdAt: statuses.createdAt,
    updatedAt: statuses.updatedAt,
  },
  defaultSort: 'sortOrder',
};

// ---------------------------------------------------------------------------

const Environment = component('Environment', z.object(lookupDto));

const environmentConfig: SimpleResourceConfig<typeof lookupSortFields> = {
  table: environments,
  entityType: 'environments',
  label: 'Environment',
  basePath: '/api/v1/environments',
  tag: 'Environments',
  names: { singular: 'environment', plural: 'environments' },
  dto: Environment,
  createBody: z.strictObject(lookupWritable),
  updateBody: nonEmptyPatch(z.strictObject(lookupWritable).partial()),
  filterShape: { isActive: QueryBool.optional() },
  filters: (q) => [activeFilter(environments.isActive)(q.isActive)],
  searchColumns: [environments.key, environments.name, environments.description],
  sortFields: lookupSortFields,
  sortColumns: {
    sortOrder: environments.sortOrder,
    name: environments.name,
    key: environments.key,
    createdAt: environments.createdAt,
    updatedAt: environments.updatedAt,
  },
  defaultSort: 'sortOrder',
};

// ---------------------------------------------------------------------------

const Location = component(
  'Location',
  z.object({
    ...lookupDto,
    parentId: Uuid.nullable().describe('Parent location (region > site > building > floor > room > rack)'),
    locationType: z.enum(LOCATION_TYPES),
    address: z.string().nullable(),
  }),
);
const locationWritable = {
  ...lookupWritable,
  parentId: Uuid.nullable().optional(),
  locationType: z.enum(LOCATION_TYPES),
  address: z.string().max(1000).nullable().optional(),
};

const locationConfig: SimpleResourceConfig<typeof lookupSortFields> = {
  table: locations,
  entityType: 'locations',
  label: 'Location',
  basePath: '/api/v1/locations',
  tag: 'Locations',
  names: { singular: 'location', plural: 'locations' },
  dto: Location,
  createBody: z.strictObject(locationWritable),
  updateBody: nonEmptyPatch(z.strictObject(locationWritable).partial()),
  filterShape: {
    isActive: QueryBool.optional(),
    parentId: z
      .union([z.literal('none'), Uuid])
      .optional()
      .describe('Children of this location; "none" for top-level locations'),
    locationType: z.enum(LOCATION_TYPES).optional(),
  },
  filters: (q) => [
    activeFilter(locations.isActive)(q.isActive),
    q.parentId === 'none' ? isNull(locations.parentId) : q.parentId ? eq(locations.parentId, q.parentId as string) : undefined,
    q.locationType ? eq(locations.locationType, q.locationType as (typeof LOCATION_TYPES)[number]) : undefined,
  ],
  searchColumns: [locations.key, locations.name, locations.description, locations.address],
  sortFields: lookupSortFields,
  sortColumns: {
    sortOrder: locations.sortOrder,
    name: locations.name,
    key: locations.key,
    createdAt: locations.createdAt,
    updatedAt: locations.updatedAt,
  },
  defaultSort: 'name',
};

// ---------------------------------------------------------------------------

const Owner = component(
  'Owner',
  z.object({
    id: Uuid,
    kind: z.enum(OWNER_KINDS),
    name: z.string(),
    email: z.string().nullable(),
    externalRef: z.string().nullable().describe('Identifier in an external directory (LDAP DN, IdP subject, HR id)'),
    isActive: z.boolean(),
    createdAt: Timestamp,
    updatedAt: Timestamp,
  }),
);
const ownerWritable = {
  kind: z.enum(OWNER_KINDS),
  name: Name,
  email: z.email().max(320).nullable().optional(),
  externalRef: z.string().trim().min(1).max(500).nullable().optional(),
  isActive: z.boolean().optional(),
};
const ownerSortFields = ['name', 'kind', 'email', 'createdAt', 'updatedAt'] as const;

const ownerConfig: SimpleResourceConfig<typeof ownerSortFields> = {
  table: owners,
  entityType: 'owners',
  label: 'Owner',
  basePath: '/api/v1/owners',
  tag: 'Owners',
  names: { singular: 'owner', plural: 'owners' },
  dto: Owner,
  createBody: z.strictObject(ownerWritable),
  updateBody: nonEmptyPatch(z.strictObject(ownerWritable).partial()),
  filterShape: {
    isActive: QueryBool.optional(),
    kind: z.enum(OWNER_KINDS).optional(),
    id: QueryUuidList.optional().describe('Only these owners (comma-separated ids)'),
  },
  filters: (q) => [
    activeFilter(owners.isActive)(q.isActive),
    q.kind ? eq(owners.kind, q.kind as (typeof OWNER_KINDS)[number]) : undefined,
    q.id ? inArray(owners.id, q.id as string[]) : undefined,
  ],
  searchColumns: [owners.name, owners.email, owners.externalRef],
  sortFields: ownerSortFields,
  sortColumns: {
    name: owners.name,
    kind: owners.kind,
    email: owners.email,
    createdAt: owners.createdAt,
    updatedAt: owners.updatedAt,
  },
  defaultSort: 'name',
};

export function lookupRoutes(db: Database): RouteSpec[] {
  const configs = [statusConfig, environmentConfig, locationConfig, ownerConfig] as SimpleResourceConfig<
    readonly [string, ...string[]]
  >[];
  return configs.flatMap((cfg) => simpleResourceRoutes(new SimpleResourceService(db, cfg), cfg));
}
