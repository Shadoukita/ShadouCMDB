import { fileURLToPath } from 'node:url';
import { eq, sql } from 'drizzle-orm';
import { loadEnv } from '../config/env.js';
import { createDb, createPool, type Database } from './client.js';
import {
  type AttributeDataType,
  auditLog,
  ciAttributeDefinitions,
  ciAttributeValues,
  ciClasses,
  ciRelationships,
  configurationItems,
  environments,
  locations,
  owners,
  relationshipTypeRules,
  relationshipTypes,
  statuses,
} from './schema/index.js';

/*
 * Reference data every installation needs. Idempotent: rows are matched by
 * their stable `key` and never overwritten, so an operator's renames survive a
 * re-run. `--demo` additionally loads a small sample inventory into an empty
 * database for API/UI development.
 */

const STATUSES = [
  { key: 'planned', name: 'Planned', isOperational: false, description: 'Approved but not yet deployed' },
  { key: 'in_service', name: 'In service', isOperational: true, description: 'Deployed and serving its purpose' },
  { key: 'maintenance', name: 'Maintenance', isOperational: true, description: 'Temporarily degraded or under maintenance' },
  { key: 'retired', name: 'Retired', isOperational: false, description: 'Decommissioned, kept for records' },
  { key: 'disposed', name: 'Disposed', isOperational: false, description: 'Physically disposed or destroyed' },
];

const ENVIRONMENTS = [
  { key: 'production', name: 'Production' },
  { key: 'staging', name: 'Staging' },
  { key: 'test', name: 'Test' },
  { key: 'development', name: 'Development' },
  { key: 'disaster_recovery', name: 'Disaster recovery' },
];

const LOCATIONS: { key: string; name: string; locationType: (typeof locations.$inferInsert)['locationType']; parent?: string; address?: string }[] = [
  { key: 'emea', name: 'EMEA', locationType: 'region' },
  { key: 'fra1', name: 'Frankfurt DC 1', locationType: 'site', parent: 'emea', address: 'Frankfurt am Main, DE' },
  { key: 'fra1_room_101', name: 'FRA1 Room 101', locationType: 'room', parent: 'fra1' },
  { key: 'fra1_rack_a01', name: 'FRA1 Rack A01', locationType: 'rack', parent: 'fra1_room_101' },
  { key: 'amer', name: 'Americas', locationType: 'region' },
  { key: 'nyc1', name: 'New York DC 1', locationType: 'site', parent: 'amer', address: 'New York, NY, US' },
  { key: 'aws_eu_central_1', name: 'AWS eu-central-1', locationType: 'cloud_region', parent: 'emea' },
];

type AttrSeed = {
  key: string;
  label: string;
  dataType: AttributeDataType;
  enumValues?: string[];
  referenceClass?: string;
  isRequired?: boolean;
  groupName?: string;
  validation?: Record<string, unknown>;
};
type ClassSeed = { key: string; name: string; parent?: string; isAbstract?: boolean; description: string; attributes: AttrSeed[] };

const OS_FAMILIES = ['linux', 'windows', 'bsd', 'unix', 'other'];

const CLASSES: ClassSeed[] = [
  {
    key: 'hardware',
    name: 'Hardware',
    isAbstract: true,
    description: 'Any physical device',
    attributes: [
      { key: 'manufacturer', label: 'Manufacturer', dataType: 'text', groupName: 'Hardware' },
      { key: 'model', label: 'Model', dataType: 'text', groupName: 'Hardware' },
      { key: 'asset_tag', label: 'Asset tag', dataType: 'text', groupName: 'Asset' },
      { key: 'purchase_date', label: 'Purchase date', dataType: 'date', groupName: 'Asset' },
      { key: 'warranty_end', label: 'Warranty end', dataType: 'date', groupName: 'Asset' },
    ],
  },
  {
    key: 'server',
    name: 'Server',
    parent: 'hardware',
    description: 'Physical server',
    attributes: [
      { key: 'cpu_cores', label: 'CPU cores', dataType: 'integer', groupName: 'Compute', validation: { min: 1 } },
      { key: 'memory_gb', label: 'Memory (GB)', dataType: 'number', groupName: 'Compute', validation: { min: 0 } },
      { key: 'os_family', label: 'OS family', dataType: 'enum', enumValues: OS_FAMILIES, groupName: 'Software' },
      { key: 'os_version', label: 'OS version', dataType: 'text', groupName: 'Software' },
      { key: 'management_ip', label: 'Management IP (BMC)', dataType: 'ip', groupName: 'Network' },
    ],
  },
  {
    key: 'network_device',
    name: 'Network device',
    parent: 'hardware',
    description: 'Switch, router, firewall, load balancer or access point',
    attributes: [
      {
        key: 'device_role',
        label: 'Role',
        dataType: 'enum',
        enumValues: ['switch', 'router', 'firewall', 'load_balancer', 'wireless_ap', 'other'],
        isRequired: true,
        groupName: 'Network',
      },
      { key: 'port_count', label: 'Port count', dataType: 'integer', groupName: 'Network', validation: { min: 0 } },
      { key: 'firmware_version', label: 'Firmware version', dataType: 'text', groupName: 'Software' },
      { key: 'management_subnet', label: 'Management subnet', dataType: 'cidr', groupName: 'Network' },
    ],
  },
  {
    key: 'virtual_machine',
    name: 'Virtual machine',
    description: 'Virtual machine or cloud instance',
    attributes: [
      { key: 'vcpu', label: 'vCPU', dataType: 'integer', groupName: 'Compute', validation: { min: 1 } },
      { key: 'memory_gb', label: 'Memory (GB)', dataType: 'number', groupName: 'Compute', validation: { min: 0 } },
      { key: 'os_family', label: 'OS family', dataType: 'enum', enumValues: OS_FAMILIES, groupName: 'Software' },
      {
        key: 'platform',
        label: 'Platform',
        dataType: 'enum',
        enumValues: ['vmware', 'hyper_v', 'kvm', 'aws', 'azure', 'gcp', 'other'],
        groupName: 'Compute',
      },
      { key: 'instance_id', label: 'Instance ID', dataType: 'text', groupName: 'Compute' },
    ],
  },
  {
    key: 'application',
    name: 'Application',
    description: 'Deployed software application',
    attributes: [
      { key: 'version', label: 'Version', dataType: 'text' },
      { key: 'vendor', label: 'Vendor', dataType: 'text' },
      { key: 'url', label: 'URL', dataType: 'text', validation: { pattern: '^https?://' } },
      { key: 'criticality', label: 'Criticality', dataType: 'enum', enumValues: ['low', 'medium', 'high', 'critical'] },
      { key: 'primary_database', label: 'Primary database', dataType: 'reference', referenceClass: 'database' },
    ],
  },
  {
    key: 'database',
    name: 'Database',
    description: 'Database instance or schema',
    attributes: [
      {
        key: 'engine',
        label: 'Engine',
        dataType: 'enum',
        enumValues: ['postgresql', 'mysql', 'mariadb', 'sql_server', 'oracle', 'mongodb', 'redis', 'other'],
        isRequired: true,
      },
      { key: 'engine_version', label: 'Engine version', dataType: 'text' },
      { key: 'port', label: 'Port', dataType: 'integer', validation: { min: 1, max: 65535 } },
      { key: 'size_gb', label: 'Size (GB)', dataType: 'number', validation: { min: 0 } },
      { key: 'backup_enabled', label: 'Backups enabled', dataType: 'boolean' },
    ],
  },
  {
    key: 'service',
    name: 'Service',
    description: 'Business or technical service offered to users',
    attributes: [
      { key: 'service_tier', label: 'Service tier', dataType: 'enum', enumValues: ['tier_1', 'tier_2', 'tier_3'] },
      { key: 'sla_uptime_percent', label: 'SLA uptime (%)', dataType: 'number', validation: { min: 0, max: 100 } },
      { key: 'support_url', label: 'Support URL', dataType: 'text' },
      { key: 'go_live_at', label: 'Go-live', dataType: 'datetime' },
    ],
  },
  {
    key: 'location',
    name: 'Location',
    description: 'A place that participates in the relationship graph; set the core location field to the matching locations row',
    attributes: [
      { key: 'rack_units', label: 'Rack units', dataType: 'integer', validation: { min: 1 } },
      { key: 'power_kw', label: 'Power budget (kW)', dataType: 'number', validation: { min: 0 } },
    ],
  },
];

const RELATIONSHIP_TYPES = [
  { key: 'runs_on', name: 'Runs on', forwardLabel: 'runs on', reverseLabel: 'hosts', isDirectional: true },
  { key: 'depends_on', name: 'Depends on', forwardLabel: 'depends on', reverseLabel: 'is required by', isDirectional: true },
  { key: 'located_in', name: 'Located in', forwardLabel: 'is located in', reverseLabel: 'contains', isDirectional: true },
  { key: 'connected_to', name: 'Connected to', forwardLabel: 'is connected to', reverseLabel: 'is connected to', isDirectional: false },
];

// [type, source class, target class]; rules also match descendant classes.
const RELATIONSHIP_RULES: [string, string, string][] = [
  ['runs_on', 'application', 'server'],
  ['runs_on', 'application', 'virtual_machine'],
  ['runs_on', 'database', 'server'],
  ['runs_on', 'database', 'virtual_machine'],
  ['runs_on', 'virtual_machine', 'server'],
  ['depends_on', 'application', 'database'],
  ['depends_on', 'application', 'application'],
  ['depends_on', 'service', 'application'],
  ['depends_on', 'service', 'service'],
  ['located_in', 'hardware', 'location'],
  ['located_in', 'location', 'location'],
  ['connected_to', 'hardware', 'hardware'],
];

const OWNERS = [
  { kind: 'team' as const, name: 'Infrastructure', email: 'infra@example.com', externalRef: 'seed:team:infrastructure' },
  { kind: 'team' as const, name: 'Platform Engineering', email: 'platform@example.com', externalRef: 'seed:team:platform' },
  { kind: 'team' as const, name: 'Database Administration', email: 'dba@example.com', externalRef: 'seed:team:dba' },
];

async function keyMap(db: Pick<Database, 'select'>, table: typeof ciClasses | typeof statuses | typeof environments | typeof locations | typeof relationshipTypes) {
  const rows = await db.select({ id: table.id, key: table.key }).from(table);
  return new Map(rows.map((r) => [r.key, r.id]));
}

function must<T>(map: Map<string, T>, key: string): T {
  const v = map.get(key);
  if (v === undefined) throw new Error(`seed: unknown key "${key}"`);
  return v;
}

export async function seedReferenceData(db: Database): Promise<void> {
  await db.transaction(async (tx) => {
    await tx.insert(statuses).values(STATUSES.map((s, i) => ({ ...s, sortOrder: i * 10 }))).onConflictDoNothing();
    await tx.insert(environments).values(ENVIRONMENTS.map((e, i) => ({ ...e, sortOrder: i * 10 }))).onConflictDoNothing();

    // Locations and classes reference their parent, so insert parents first.
    for (const loc of LOCATIONS) {
      const parentId = loc.parent ? must(await keyMap(tx, locations), loc.parent) : null;
      await tx
        .insert(locations)
        .values({ key: loc.key, name: loc.name, locationType: loc.locationType, address: loc.address, parentId })
        .onConflictDoNothing();
    }

    for (const cls of CLASSES) {
      const parentId = cls.parent ? must(await keyMap(tx, ciClasses), cls.parent) : null;
      await tx
        .insert(ciClasses)
        .values({ key: cls.key, name: cls.name, description: cls.description, isAbstract: cls.isAbstract ?? false, parentId })
        .onConflictDoNothing();
    }
    const classIds = await keyMap(tx, ciClasses);
    for (const cls of CLASSES) {
      await tx
        .insert(ciAttributeDefinitions)
        .values(
          cls.attributes.map((a, i) => ({
            classId: must(classIds, cls.key),
            key: a.key,
            label: a.label,
            dataType: a.dataType,
            isRequired: a.isRequired ?? false,
            enumValues: a.enumValues ?? null,
            referenceClassId: a.referenceClass ? must(classIds, a.referenceClass) : null,
            validation: a.validation ?? null,
            groupName: a.groupName ?? null,
            sortOrder: i * 10,
          })),
        )
        .onConflictDoNothing();
    }

    await tx
      .insert(relationshipTypes)
      .values(RELATIONSHIP_TYPES.map((r, i) => ({ ...r, sortOrder: i * 10 })))
      .onConflictDoNothing();
    const typeIds = await keyMap(tx, relationshipTypes);
    await tx
      .insert(relationshipTypeRules)
      .values(
        RELATIONSHIP_RULES.map(([type, source, target]) => ({
          relationshipTypeId: must(typeIds, type),
          sourceClassId: must(classIds, source),
          targetClassId: must(classIds, target),
        })),
      )
      .onConflictDoNothing();

    await tx.insert(owners).values(OWNERS).onConflictDoNothing();
  });
}

/** Small sample inventory; only loaded into a database with no CIs. */
export async function seedDemoData(db: Database): Promise<boolean> {
  const [{ n } = { n: 0 }] = await db.select({ n: sql<number>`count(*)::int` }).from(configurationItems);
  if (n > 0) return false;

  await db.transaction(async (tx) => {
    const cls = await keyMap(tx, ciClasses);
    const st = await keyMap(tx, statuses);
    const envs = await keyMap(tx, environments);
    const locs = await keyMap(tx, locations);
    const types = await keyMap(tx, relationshipTypes);
    const ownerRows = await tx.select({ id: owners.id, ref: owners.externalRef }).from(owners);
    const owner = (ref: string) => must(new Map(ownerRows.map((o) => [o.ref ?? '', o.id])), ref);

    const inService = must(st, 'in_service');
    const prod = must(envs, 'production');
    const infra = owner('seed:team:infrastructure');

    const ci = async (values: Omit<typeof configurationItems.$inferInsert, 'statusId'> & { statusId?: string }) => {
      const [row] = await tx
        .insert(configurationItems)
        .values({ statusId: inService, ...values })
        .returning({ id: configurationItems.id });
      if (!row) throw new Error('seed: insert returned no row');
      await tx.insert(auditLog).values({
        actorType: 'system',
        actorName: 'seed',
        action: 'create',
        entityType: 'configuration_items',
        entityId: row.id,
        newValue: { ...values, id: row.id },
      });
      return row.id;
    };

    const rack = await ci({ classId: must(cls, 'location'), name: 'FRA1 Rack A01', locationId: must(locs, 'fra1_rack_a01'), ownerId: infra });
    const srv = await ci({
      classId: must(cls, 'server'), name: 'fra1-esx-01', hostname: 'fra1-esx-01.example.internal', ipAddress: '10.10.1.11',
      serialNumber: 'SN-DL380-0001', environmentId: prod, ownerId: infra, locationId: must(locs, 'fra1_rack_a01'),
    });
    const sw = await ci({
      classId: must(cls, 'network_device'), name: 'fra1-tor-a01', hostname: 'fra1-tor-a01.example.internal', ipAddress: '10.10.0.2',
      serialNumber: 'SN-N9K-0042', environmentId: prod, ownerId: infra, locationId: must(locs, 'fra1_rack_a01'),
    });
    const vm = await ci({
      classId: must(cls, 'virtual_machine'), name: 'crm-app-01', hostname: 'crm-app-01.example.internal', ipAddress: '10.20.5.21',
      environmentId: prod, ownerId: owner('seed:team:platform'), locationId: must(locs, 'fra1'),
    });
    const db1 = await ci({
      classId: must(cls, 'database'), name: 'crm-db', hostname: 'crm-db-01.example.internal', ipAddress: '10.20.6.31',
      environmentId: prod, ownerId: owner('seed:team:dba'), locationId: must(locs, 'fra1'),
    });
    const app = await ci({ classId: must(cls, 'application'), name: 'CRM', environmentId: prod, ownerId: owner('seed:team:platform') });
    const svc = await ci({ classId: must(cls, 'service'), name: 'Customer Relationship Management', environmentId: prod, ownerId: owner('seed:team:platform') });
    await ci({ classId: must(cls, 'server'), name: 'nyc1-old-01', statusId: must(st, 'retired'), locationId: must(locs, 'nyc1'), serialNumber: 'SN-OLD-0007' });

    const defs = await tx
      .select({ id: ciAttributeDefinitions.id, key: ciAttributeDefinitions.key, classKey: ciClasses.key })
      .from(ciAttributeDefinitions)
      .innerJoin(ciClasses, eq(ciClasses.id, ciAttributeDefinitions.classId));
    const attr = (classKey: string, key: string) => {
      const d = defs.find((x) => x.classKey === classKey && x.key === key);
      if (!d) throw new Error(`seed: unknown attribute ${classKey}.${key}`);
      return d.id;
    };
    await tx.insert(ciAttributeValues).values([
      { ciId: srv, attributeId: attr('hardware', 'manufacturer'), valueText: 'HPE' },
      { ciId: srv, attributeId: attr('hardware', 'model'), valueText: 'ProLiant DL380 Gen10' },
      { ciId: srv, attributeId: attr('hardware', 'warranty_end'), valueDate: '2028-03-31' },
      { ciId: srv, attributeId: attr('server', 'cpu_cores'), valueNumber: '32' },
      { ciId: srv, attributeId: attr('server', 'memory_gb'), valueNumber: '512' },
      { ciId: srv, attributeId: attr('server', 'os_family'), valueText: 'other' },
      { ciId: srv, attributeId: attr('server', 'management_ip'), valueIp: '10.10.100.11' },
      { ciId: sw, attributeId: attr('hardware', 'manufacturer'), valueText: 'Cisco' },
      { ciId: sw, attributeId: attr('network_device', 'device_role'), valueText: 'switch' },
      { ciId: sw, attributeId: attr('network_device', 'port_count'), valueNumber: '48' },
      { ciId: sw, attributeId: attr('network_device', 'management_subnet'), valueCidr: '10.10.0.0/24' },
      { ciId: vm, attributeId: attr('virtual_machine', 'vcpu'), valueNumber: '8' },
      { ciId: vm, attributeId: attr('virtual_machine', 'memory_gb'), valueNumber: '32' },
      { ciId: vm, attributeId: attr('virtual_machine', 'os_family'), valueText: 'linux' },
      { ciId: vm, attributeId: attr('virtual_machine', 'platform'), valueText: 'vmware' },
      { ciId: db1, attributeId: attr('database', 'engine'), valueText: 'postgresql' },
      { ciId: db1, attributeId: attr('database', 'engine_version'), valueText: '17.2' },
      { ciId: db1, attributeId: attr('database', 'port'), valueNumber: '5432' },
      { ciId: db1, attributeId: attr('database', 'backup_enabled'), valueBoolean: true },
      { ciId: app, attributeId: attr('application', 'version'), valueText: '4.2.0' },
      { ciId: app, attributeId: attr('application', 'criticality'), valueText: 'high' },
      { ciId: app, attributeId: attr('application', 'primary_database'), valueRefCiId: db1 },
      { ciId: svc, attributeId: attr('service', 'service_tier'), valueText: 'tier_1' },
      { ciId: svc, attributeId: attr('service', 'sla_uptime_percent'), valueNumber: '99.9' },
    ]);

    const rel = (type: string, sourceCiId: string, targetCiId: string) => ({ relationshipTypeId: must(types, type), sourceCiId, targetCiId });
    await tx.insert(ciRelationships).values([
      rel('depends_on', svc, app), // Service -> Application
      rel('runs_on', app, vm), // Application -> VM
      rel('depends_on', app, db1), // Application -> Database
      rel('runs_on', vm, srv), // VM -> Server
      rel('runs_on', db1, vm), // Database -> VM
      rel('located_in', srv, rack), // Device -> Location
      rel('located_in', sw, rack),
      rel('connected_to', srv, sw),
    ]);
  });
  return true;
}

async function main(): Promise<void> {
  const env = loadEnv();
  const pool = createPool(env);
  const db = createDb(pool);
  try {
    await seedReferenceData(db);
    const counts = await db.execute<{ table: string; n: number }>(sql`
      SELECT 'ci_classes' AS table, count(*)::int AS n FROM ci_classes
      UNION ALL SELECT 'ci_attribute_definitions', count(*)::int FROM ci_attribute_definitions
      UNION ALL SELECT 'statuses', count(*)::int FROM statuses
      UNION ALL SELECT 'environments', count(*)::int FROM environments
      UNION ALL SELECT 'locations', count(*)::int FROM locations
      UNION ALL SELECT 'owners', count(*)::int FROM owners
      UNION ALL SELECT 'relationship_types', count(*)::int FROM relationship_types
      UNION ALL SELECT 'relationship_type_rules', count(*)::int FROM relationship_type_rules`);
    console.log('Reference data:', Object.fromEntries(counts.rows.map((r) => [r.table, r.n])));

    if (process.argv.includes('--demo')) {
      const loaded = await seedDemoData(db);
      console.log(loaded ? 'Demo inventory loaded.' : 'Demo inventory skipped: database already contains CIs.');
    }
  } finally {
    await pool.end();
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch((err: unknown) => {
    console.error('Seed failed:', err instanceof Error ? err.message : err);
    process.exit(1);
  });
}
