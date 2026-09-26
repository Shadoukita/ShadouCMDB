import { sql } from 'drizzle-orm';
import { type AnyPgColumn, boolean, check, index, pgTable, text, uuid } from 'drizzle-orm/pg-core';
import { KEY_PATTERN, id, lookupColumns, timestamps } from './common.js';

/** Lifecycle status of a CI (planned, in_service, retired, ...). */
export const statuses = pgTable(
  'statuses',
  {
    ...lookupColumns(),
    // True for statuses that count as "live" in reports (in_service, maintenance).
    isOperational: boolean('is_operational').notNull().default(false),
  },
  (t) => [check('statuses_key_format', sql`${t.key} ~ ${sql.raw(`'${KEY_PATTERN}'`)}`)],
);

/** Deployment environment (production, staging, development, ...). */
export const environments = pgTable(
  'environments',
  {
    ...lookupColumns(),
  },
  (t) => [check('environments_key_format', sql`${t.key} ~ ${sql.raw(`'${KEY_PATTERN}'`)}`)],
);

export const LOCATION_TYPES = ['region', 'site', 'building', 'floor', 'room', 'rack', 'cloud_region', 'other'] as const;

/** Physical/logical location hierarchy: region > site > building > room > rack. */
export const locations = pgTable(
  'locations',
  {
    ...lookupColumns(),
    parentId: uuid('parent_id').references((): AnyPgColumn => locations.id, { onDelete: 'restrict' }),
    locationType: text('location_type', { enum: LOCATION_TYPES }).notNull(),
    address: text('address'),
  },
  (t) => [
    check('locations_key_format', sql`${t.key} ~ ${sql.raw(`'${KEY_PATTERN}'`)}`),
    check('locations_not_own_parent', sql`${t.parentId} IS NULL OR ${t.parentId} <> ${t.id}`),
    check(
      'locations_type_valid',
      sql`${t.locationType} IN (${sql.raw(LOCATION_TYPES.map((v) => `'${v}'`).join(', '))})`,
    ),
    index('locations_parent_idx').on(t.parentId),
  ],
);

export const OWNER_KINDS = ['person', 'team'] as const;

/**
 * People or teams accountable for CIs. Deliberately not "users": there is no
 * authentication in Milestone 1. A future users/auth table can link here via
 * external_ref or a nullable FK without reshaping CI ownership.
 */
export const owners = pgTable(
  'owners',
  {
    id: id(),
    kind: text('kind', { enum: OWNER_KINDS }).notNull(),
    name: text('name').notNull(),
    email: text('email'),
    // Identifier in an external directory (LDAP DN, IdP subject, HR id) for later integrations.
    externalRef: text('external_ref').unique(),
    isActive: boolean('is_active').notNull().default(true),
    ...timestamps(),
  },
  (t) => [
    check('owners_kind_valid', sql`${t.kind} IN (${sql.raw(OWNER_KINDS.map((v) => `'${v}'`).join(', '))})`),
    check('owners_email_format', sql`${t.email} IS NULL OR ${t.email} ~ '^[^@\\s]+@[^@\\s]+$'`),
    index('owners_name_idx').on(sql`lower(${t.name})`),
  ],
);
