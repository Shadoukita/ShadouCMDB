/*
 * End-to-end smoke test against a running API (which must be connected to a
 * real, migrated and seeded PostgreSQL). It exercises every operation in the
 * OpenAPI document, including the error paths, and fails if any operation
 * was not called, any status is unexpected, anything returns 5xx, or a
 * response body does not match the schema the document declares for it.
 *
 *   API_URL=http://localhost:3000 node tools/smoke/smoke.ts   # Node.js 22.18+, no dependencies
 *
 * The API can run anywhere (a local binary, a container, a remote host); the
 * script only needs its URL. It only creates rows with a unique run suffix and
 * never deletes seed data (`shadoucmdb seed --demo` must have been run).
 *
 * Signing in: on a database without users the script completes first-run
 * setup itself (as SMOKE_USERNAME, default "smoke-admin"). Otherwise set
 * SMOKE_USERNAME and SMOKE_PASSWORD to an account holding the Administrator
 * profile.
 */

const BASE = (process.env.API_URL ?? '').replace(/\/$/, '');
if (!BASE) {
  console.error('Set API_URL, e.g. API_URL=http://localhost:3000');
  process.exit(2);
}
const RUN = Date.now().toString(36);
const VERBOSE = process.argv.includes('--verbose');
const ADMIN_USERNAME = process.env.SMOKE_USERNAME ?? 'smoke-admin';
const ADMIN_PASSWORD = process.env.SMOKE_PASSWORD ?? `smoke-${RUN}-password`;

/** A signed-in user: the session cookie and the CSRF token that goes with it. */
interface Identity {
  name: string;
  cookie: string;
  csrf: string;
}
let me: Identity | null = null;

/** Runs `fn` as another identity (null: anonymous), then switches back. */
async function as<T>(who: Identity | null, fn: () => Promise<T>): Promise<T> {
  const previous = me;
  me = who;
  try {
    return await fn();
  } finally {
    me = previous;
  }
}

type Json = any; // eslint-disable-line @typescript-eslint/no-explicit-any

interface Op {
  method: string;
  path: string;
  operationId: string;
  description: string;
  regex: RegExp;
  responses: Record<string, Json>;
}
let ops: Op[] = [];
let schemas: Record<string, Json> = {};
const covered = new Set<string>();
const failures: string[] = [];
let calls = 0;
let checkedBodies = 0;

function operationFor(method: string, url: string): Op | undefined {
  const path = url.split('?')[0]!;
  return ops.find((o) => o.method === method && o.regex.test(path));
}

// --- Response bodies against the declared schemas (the JSON Schema subset the spec uses) ---
const typeOf = (v: unknown) => (v === null ? 'null' : Array.isArray(v) ? 'array' : typeof v === 'number' && Number.isInteger(v) ? 'integer' : typeof v);

function schemaErrors(schema: Json, value: unknown, path: string, out: string[]): void {
  if (!schema || typeof schema !== 'object') return;
  if (schema.$ref) return schemaErrors(schemas[String(schema.$ref).split('/').pop()!], value, path, out);
  for (const key of ['anyOf', 'oneOf'] as const) {
    if (Array.isArray(schema[key])) {
      const results = schema[key].map((s: Json) => {
        const e: string[] = [];
        schemaErrors(s, value, path, e);
        return e;
      });
      if (!results.some((e: string[]) => e.length === 0)) out.push(`${path}: matches none of ${key} (${results.map((e: string[]) => e[0]).join(' / ')})`);
      return;
    }
  }
  if (schema.type) {
    const types: string[] = Array.isArray(schema.type) ? schema.type : [schema.type];
    const t = typeOf(value);
    if (!types.includes(t) && !(t === 'integer' && types.includes('number'))) return void out.push(`${path}: expected ${types.join('|')}, got ${t}`);
  }
  if (schema.enum && !schema.enum.some((e: unknown) => e === value)) out.push(`${path}: ${JSON.stringify(value)} not in enum`);
  if ('const' in schema && schema.const !== value) out.push(`${path}: expected ${JSON.stringify(schema.const)}`);
  if (typeOf(value) === 'array' && schema.items) (value as unknown[]).forEach((v, i) => schemaErrors(schema.items, v, `${path}[${i}]`, out));
  if (typeOf(value) === 'object') {
    const obj = value as Record<string, unknown>;
    for (const r of schema.required ?? []) if (!(r in obj)) out.push(`${path}.${r}: required but missing`);
    for (const [k, v] of Object.entries(obj)) {
      if (schema.properties?.[k]) schemaErrors(schema.properties[k], v, `${path}.${k}`, out);
      else if (schema.additionalProperties === false) out.push(`${path}.${k}: not declared in the schema`);
      else if (typeof schema.additionalProperties === 'object') schemaErrors(schema.additionalProperties, v, `${path}.${k}`, out);
    }
  }
}

function checkResponse(op: Op, status: number, json: unknown): void {
  const declared = op.responses[String(status)];
  if (!declared) return void failures.push(`${op.operationId}: status ${status} is not documented`);
  const schema = declared.content?.['application/json']?.schema;
  if (!schema) return;
  checkedBodies++;
  const errors: string[] = [];
  schemaErrors(schema, json, '$', errors);
  if (errors.length) {
    console.log(`FAIL schema: ${op.operationId} ${status}: ${errors.slice(0, 3).join('; ')}`);
    failures.push(`${op.operationId} ${status} response does not match its schema: ${errors.slice(0, 5).join('; ')}`);
  }
}

async function call(
  method: string,
  url: string,
  body?: unknown,
  expect?: number,
  headers: Record<string, string> = {},
  opts: { cover?: boolean; accept?: number[] } = {},
): Promise<{ status: number; json: Json; headers: Headers; bytes: Uint8Array }> {
  const res = await fetch(BASE + url, {
    method,
    headers: {
      ...(me ? { cookie: me.cookie } : {}),
      ...(me && method !== 'GET' ? { 'x-csrf-token': me.csrf } : {}),
      ...(body !== undefined ? { 'content-type': 'application/json' } : {}),
      ...headers,
    },
    body: body === undefined ? undefined : typeof body === 'string' ? body : JSON.stringify(body),
  });
  const bytes = new Uint8Array(await res.arrayBuffer());
  const binary = /^image\//.test(res.headers.get('content-type') ?? '');
  const text = binary ? '' : new TextDecoder().decode(bytes);
  const json = text ? JSON.parse(text) : undefined;
  calls++;
  const op = operationFor(method, url);
  if (op) {
    if (opts.cover !== false) covered.add(op.operationId);
    checkResponse(op, res.status, json);
  }
  const ok = (opts.accept ? opts.accept.includes(res.status) : expect === undefined ? res.status < 400 : res.status === expect) && res.status < 500;
  const summary = summarise(json);
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${method.padEnd(6)} ${url} -> ${res.status}${summary ? `  ${summary}` : ''}`);
  if (VERBOSE && json) console.log(JSON.stringify(json, null, 1).slice(0, 2000));
  if (!ok) failures.push(`${method} ${url}: expected ${expect ?? '2xx'}, got ${res.status} ${text.slice(0, 400)}`);
  return { status: res.status, json, headers: res.headers, bytes };
}

/** The session and CSRF cookies from a login or setup response. */
function identityFrom(name: string, res: { headers: Headers; json: Json }): Identity {
  const cookies = res.headers.getSetCookie().map((c) => c.split(';')[0]!);
  const session = cookies.find((c) => c.startsWith('shadoucmdb_session='));
  check(session && cookies.some((c) => c.startsWith('shadoucmdb_csrf=')), `${name}: session and CSRF cookies are set`);
  check(res.headers.getSetCookie().some((c) => /shadoucmdb_session=.*HttpOnly/.test(c) && /SameSite=Lax/.test(c)), `${name}: session cookie is HttpOnly and SameSite=Lax`);
  return { name, cookie: session ?? '', csrf: res.json?.csrfToken ?? '' };
}

async function login(username: string, password: string): Promise<Identity> {
  const res = await as(null, () => post('/api/v1/auth/login', { username, password }, 200));
  return identityFrom(username, res);
}

const loginFails = (username: string, password: string) => as(null, () => post('/api/v1/auth/login', { username, password }, 401));

function summarise(j: Json): string {
  if (!j) return '';
  if (j.error) {
    const d = (j.error.details ?? []).map((x: Json) => `${x.in}.${x.field}: ${x.message}`).join('; ');
    return `${j.error.code}${d ? ` [${d}]` : ` "${j.error.message}"`}`;
  }
  if (j.page) return `total=${j.page.total} returned=${j.data.length}${j.data[0]?.name ? ` first="${j.data[0].name ?? ''}"` : ''}`;
  if (j.nodes) return `nodes=${j.nodes.length} edges=${j.edges.length} truncated=${j.truncated}`;
  if (j.status) return JSON.stringify(j);
  if (j.data) return `items=${j.data.length}`;
  if (j.id) return `id=${String(j.id).slice(0, 8)} ${j.key ?? j.name ?? j.type?.key ?? ''}`.trim();
  return '';
}

function check(cond: unknown, message: string) {
  if (!cond) {
    failures.push(message);
    console.log(`FAIL check: ${message}`);
  } else if (VERBOSE) console.log(`ok   check: ${message}`);
}

const get = (url: string, expect?: number) => call('GET', url, undefined, expect);
const post = (url: string, body: unknown, expect = 201) => call('POST', url, body, expect);
const patch = (url: string, body: unknown, expect = 200) => call('PATCH', url, body, expect);
const del = (url: string, expect = 204) => call('DELETE', url, undefined, expect);

async function idByKey(collection: string, key: string): Promise<string> {
  const { json } = await get(`/api/v1/${collection}?limit=200&q=${key}`);
  const row = json.data.find((r: Json) => r.key === key);
  if (!row) throw new Error(`seed row ${collection}/${key} not found; run \`shadoucmdb seed --demo\` first`);
  return row.id;
}

async function main() {
  // --- OpenAPI + health --------------------------------------------------------
  const spec = (await get('/openapi.json')).json;
  ops = Object.entries(spec.paths as Record<string, Record<string, Json>>).flatMap(([path, methods]) =>
    Object.entries(methods).map(([m, op]) => ({
      method: m.toUpperCase(),
      path,
      operationId: op.operationId as string,
      description: (op.description ?? '') as string,
      regex: new RegExp(`^${path.replace(/\{[^}]+\}/g, '[^/]+')}$`),
      responses: op.responses ?? {},
    })),
  );
  schemas = spec.components?.schemas ?? {};
  console.log(`# OpenAPI ${spec.openapi}: ${ops.length} operations`);
  await get('/healthz');
  const ready = await get('/readyz');
  check(ready.json.migrations?.upToDate === true, 'readyz reports migrations up to date');

  // --- Without a session, everything but health, login and setup is 401 ----------
  console.log('\n# Unauthenticated');
  const PUBLIC = ['getLiveness', 'getReadiness', 'getSetupStatus', 'completeSetup', 'login', 'loginSecondFactor', 'getPublicBranding', 'getUiAsset'];
  const specPublic = Object.values(spec.paths as Record<string, Record<string, Json>>)
    .flatMap((m) => Object.values(m))
    .filter((op) => Array.isArray(op.security) && op.security.length === 0)
    .map((op) => op.operationId)
    .sort();
  check(JSON.stringify(specPublic) === JSON.stringify([...PUBLIC].sort()), `only ${PUBLIC.join(', ')} are public in the spec (got ${specPublic.join(', ')})`);
  const anyId = '00000000-0000-4000-8000-000000000000';
  let sweep = 0;
  for (const op of ops.filter((o) => !PUBLIC.includes(o.operationId))) {
    const url = op.path.replace(/\{[^}]+\}/g, anyId);
    const res = await call(op.method, url, op.method === 'GET' || op.method === 'DELETE' ? undefined : {}, 401, {}, { cover: false });
    if (res.json?.error?.code !== 'UNAUTHENTICATED') failures.push(`${op.operationId}: expected UNAUTHENTICATED without a session`);
    sweep++;
  }
  console.log(`# ${sweep} operations answered 401 without a session`);

  // --- First-run setup or sign-in ---------------------------------------------------
  console.log('\n# Setup / sign-in');
  const setupStatus = (await get('/api/v1/setup')).json;
  if (setupStatus.setupRequired) {
    await post('/api/v1/setup', { username: ADMIN_USERNAME, displayName: 'Smoke admin', password: 'too short' }, 400);
    // Two at once: the advisory lock lets exactly one of them create the administrator.
    const body = { username: ADMIN_USERNAME, displayName: 'Smoke admin', password: ADMIN_PASSWORD };
    const both = await Promise.all([0, 1].map(() => call('POST', '/api/v1/setup', body, undefined, {}, { accept: [201, 409] })));
    check(both.map((r) => r.status).sort().join() === '201,409', 'two concurrent setups: exactly one 201 and one 409');
    const res = both.find((r) => r.status === 201) ?? both[0]!;
    check(res.json.user?.isAdministrator === true && res.json.permissions?.administrator === true, 'setup creates an administrator and signs them in');
    me = identityFrom(ADMIN_USERNAME, res);
    check((await get('/api/v1/setup')).json.setupRequired === false, 'setup is no longer required');
  } else {
    if (!process.env.SMOKE_PASSWORD) throw new Error('This API already has users: set SMOKE_USERNAME and SMOKE_PASSWORD (an Administrator account).');
    me = await login(ADMIN_USERNAME, ADMIN_PASSWORD);
  }
  const admin = me;
  await post('/api/v1/setup', { username: `late-${RUN}`, displayName: 'Too late', password: 'correct horse battery' }, 409);
  const adminMe = (await get('/api/v1/auth/me')).json;
  check(adminMe.user.username === ADMIN_USERNAME && adminMe.csrfToken === admin.csrf, '/auth/me returns the user and the CSRF token');

  // --- Lookups ------------------------------------------------------------------
  console.log('\n# Statuses / environments / locations / owners');
  await get('/api/v1/statuses?sort=-name&isOperational=true');
  const status = (await post('/api/v1/statuses', { key: `smoke_${RUN}`, name: 'Smoke status', isOperational: false })).json;
  await get(`/api/v1/statuses/${status.id}`);
  await patch(`/api/v1/statuses/${status.id}`, { name: 'Smoke status (renamed)', sortOrder: 99 });
  await post('/api/v1/statuses', { key: `smoke_${RUN}`, name: 'dup' }, 409);
  await post('/api/v1/statuses', { key: 'Bad Key', name: '' }, 400);
  await patch(`/api/v1/statuses/${status.id}`, {}, 400);
  const inService = await idByKey('statuses', 'in_service');
  await del(`/api/v1/statuses/${inService}`, 409);
  await del(`/api/v1/statuses/${status.id}`);
  await get(`/api/v1/statuses/${status.id}`, 404);

  await get('/api/v1/environments?q=prod');
  const env = (await post('/api/v1/environments', { key: `smoke_env_${RUN}`, name: 'Smoke env' })).json;
  await get(`/api/v1/environments/${env.id}`);
  await patch(`/api/v1/environments/${env.id}`, { isActive: false });
  await del(`/api/v1/environments/${env.id}`);
  const production = await idByKey('environments', 'production');

  await get('/api/v1/locations?parentId=none&sort=name');
  const fra1 = await idByKey('locations', 'fra1');
  await get(`/api/v1/locations?parentId=${fra1}`);
  const room = (await post('/api/v1/locations', { key: `smoke_room_${RUN}`, name: 'Smoke room', locationType: 'room', parentId: fra1 })).json;
  await get(`/api/v1/locations/${room.id}`);
  await patch(`/api/v1/locations/${room.id}`, { parentId: room.id }, 400); // own parent
  await post('/api/v1/locations', { key: `smoke_x_${RUN}`, name: 'x', locationType: 'moon' }, 400);
  await post('/api/v1/locations', { key: `smoke_y_${RUN}`, name: 'y', locationType: 'room', parentId: '00000000-0000-4000-8000-000000000000' }, 400);

  await get('/api/v1/owners?kind=team&sort=name');
  const owner = (await post('/api/v1/owners', { kind: 'person', name: `Smoke Person ${RUN}`, email: `smoke-${RUN}@example.com` })).json;
  await get(`/api/v1/owners/${owner.id}`);
  await patch(`/api/v1/owners/${owner.id}`, { email: 'not-an-email' }, 400);
  await patch(`/api/v1/owners/${owner.id}`, { externalRef: `hr-${RUN}` });

  // --- Starter templates ----------------------------------------------------------
  console.log('\n# Templates');
  const templates = (await get('/api/v1/admin/templates')).json;
  const itTemplate = templates.data.find((t: Json) => t.key === 'it_infrastructure');
  check(itTemplate?.status === 'installed' && itTemplate.contents.classes === 8 && itTemplate.contents.attributeDefinitions === 35,
    'the IT infrastructure template is listed as installed (seed --demo installs it)');
  const reinstall = (await post('/api/v1/admin/templates/it_infrastructure/install', undefined, 200)).json;
  check(Object.values(reinstall.created as Record<string, number>).every((n) => n === 0) && reinstall.existing.classes === 8,
    'installing the template again changes nothing');
  await post('/api/v1/admin/templates/no_such_template/install', undefined, 404);
  await post('/api/v1/admin/templates/Not-A-Key/install', undefined, 400);

  // --- Admin-defined lookup lists ----------------------------------------------------
  console.log('\n# Lookup lists');
  const contracts = (await post('/api/v1/lookup-lists', { key: `smoke_contract_${RUN}`, name: 'Support contract' })).json;
  await get(`/api/v1/lookup-lists?q=smoke_contract_${RUN}&sort=name`);
  await get(`/api/v1/lookup-lists/${contracts.id}`);
  await patch(`/api/v1/lookup-lists/${contracts.id}`, { description: 'Vendor support level' });
  const gold = (await post('/api/v1/lookup-list-values', { listId: contracts.id, key: 'gold', name: 'Gold', color: '#d4a72c', sortOrder: 10 })).json;
  const silver = (await post('/api/v1/lookup-list-values', { listId: contracts.id, key: 'silver', name: 'Silver', sortOrder: 20 })).json;
  await post('/api/v1/lookup-list-values', { listId: contracts.id, key: 'gold', name: 'Gold again' }, 409);
  await post('/api/v1/lookup-list-values', { listId: contracts.id, key: 'bronze', name: 'Bronze', color: 'brown' }, 400);
  await get(`/api/v1/lookup-list-values?listId=${contracts.id}&sort=sortOrder`);
  await get(`/api/v1/lookup-list-values/${gold.id}`);
  await patch(`/api/v1/lookup-list-values/${silver.id}`, { name: 'Silver (8x5)' });

  // --- Classes and attribute definitions ----------------------------------------
  console.log('\n# CI classes / attribute definitions');
  const hardware = await idByKey('ci-classes', 'hardware');
  const networkDevice = await idByKey('ci-classes', 'network_device');
  const serverClass = await idByKey('ci-classes', 'server');
  const appClass = await idByKey('ci-classes', 'application');
  const dbClass = await idByKey('ci-classes', 'database');
  const infra = await idByKey('areas', 'infrastruktur');
  await get(`/api/v1/ci-classes?descendantOf=${hardware}&areaId=${infra}`);
  const lb = (await post('/api/v1/ci-classes', { key: `smoke_lb_${RUN}`, name: 'Smoke load balancer', parentId: networkDevice, areaId: infra })).json;
  check(lb.tableName === `infrastruktur.smoke_lb_${RUN}` && lb.viewName === `infrastruktur.v_smoke_lb_${RUN}`, 'a new type gets a table and a reporting view in its area');
  await get(`/api/v1/ci-classes/${lb.id}`);
  await patch(`/api/v1/ci-classes/${lb.id}`, { description: 'LB for smoke test', icon: 'scale', color: '#0a7ea4', sortOrder: 25 });
  await patch(`/api/v1/ci-classes/${lb.id}`, { color: 'teal' }, 400);
  await patch(`/api/v1/ci-classes/${hardware}`, { parentId: lb.id }, 400); // cycle
  await patch(`/api/v1/ci-classes/${lb.id}`, { key: 'renamed' }, 400); // key immutable
  const algo = (await post('/api/v1/attribute-definitions', {
    classId: lb.id, key: 'algorithm', label: 'Algorithm', dataType: 'enum', enumValues: ['round_robin', 'least_conn'], isRequired: true,
  })).json;
  const vip = (await post('/api/v1/attribute-definitions', { classId: lb.id, key: 'vip', label: 'Virtual IP', dataType: 'ip' })).json;
  const support = (await post('/api/v1/attribute-definitions', {
    classId: lb.id, key: 'support', label: 'Support contract', dataType: 'lookup', lookupListId: contracts.id,
    defaultValue: silver.id, helpText: 'Vendor support level', groupName: 'Contract',
  })).json;
  check(support.lookupListId === contracts.id && support.defaultValue === silver.id && support.helpText === 'Vendor support level',
    'lookup attribute with a default and help text');
  await post('/api/v1/attribute-definitions', { classId: lb.id, key: 'tier', label: 'Tier', dataType: 'lookup' }, 400); // no list
  await post('/api/v1/attribute-definitions', { classId: lb.id, key: 'tier', label: 'Tier', dataType: 'enum', enumValues: ['a'], defaultValue: 'b' }, 400);
  await post('/api/v1/attribute-definitions', { classId: lb.id, key: 'peer', label: 'Peer', dataType: 'reference', referenceClassId: lb.id, defaultValue: 'x' }, 400);
  await post('/api/v1/attribute-definitions', { classId: lb.id, key: 'model', label: 'Model again', dataType: 'text' }, 422); // defined on hardware
  await post('/api/v1/attribute-definitions', { classId: lb.id, key: 'mode', label: 'Mode', dataType: 'enum' }, 400); // enum w/o values
  await post('/api/v1/attribute-definitions', { classId: lb.id, key: 'w', label: 'W', dataType: 'text', validation: { min: 1 } }, 400);
  await get(`/api/v1/attribute-definitions/${vip.id}`);
  await patch(`/api/v1/attribute-definitions/${vip.id}`, { label: 'VIP', groupName: 'Network' });
  await get(`/api/v1/attribute-definitions?effectiveForClassId=${lb.id}&sort=key`);
  const eff = (await get(`/api/v1/ci-classes/${lb.id}/attributes`)).json;
  check(eff.data.some((a: Json) => a.key === 'manufacturer' && a.inherited) && eff.data.some((a: Json) => a.key === 'algorithm' && !a.inherited),
    'load balancer inherits hardware attributes and has its own');

  // --- Relationship types and rules ---------------------------------------------
  console.log('\n# Relationship types / rules');
  const runsOn = await idByKey('relationship-types', 'runs_on');
  const dependsOn = await idByKey('relationship-types', 'depends_on');
  const locatedIn = await idByKey('relationship-types', 'located_in');
  const allowed = (await get(`/api/v1/relationship-types?sourceClassId=${appClass}&targetClassId=${serverClass}`)).json;
  check(allowed.data.some((t: Json) => t.key === 'runs_on'), 'runs_on allowed application -> server');
  const rt = (await post('/api/v1/relationship-types', { key: `smoke_backs_up_${RUN}`, name: 'Backs up', forwardLabel: 'backs up', reverseLabel: 'is backed up by' })).json;
  await get(`/api/v1/relationship-types/${rt.id}`);
  await patch(`/api/v1/relationship-types/${rt.id}`, { description: 'backup relation' });
  const rule = (await post('/api/v1/relationship-rules', { relationshipTypeId: rt.id, sourceClassId: serverClass, targetClassId: dbClass })).json;
  await post('/api/v1/relationship-rules', { relationshipTypeId: rt.id, sourceClassId: serverClass, targetClassId: dbClass }, 409);
  await get(`/api/v1/relationship-rules?relationshipTypeId=${rt.id}`);
  await get(`/api/v1/relationship-rules/${rule.id}`);
  await patch(`/api/v1/relationship-rules/${rule.id}`, { targetClassId: appClass });

  // --- Configuration items ------------------------------------------------------
  console.log('\n# Configuration items');
  await get('/api/v1/configuration-items?limit=5&sort=-updatedAt');
  await get(`/api/v1/configuration-items?classId=${hardware}&statusId=${inService}&sort=className`);
  await get('/api/v1/configuration-items?ipWithin=10.0.0.0/8&sort=ipAddress');
  const server = (await post('/api/v1/configuration-items', {
    classId: serverClass, name: `smoke-srv-${RUN}`, statusId: inService, environmentId: production, ownerId: owner.id, locationId: room.id,
    hostname: `smoke-srv-${RUN}.example.internal`, ipAddress: '10.77.0.10', serialNumber: `SN-SMOKE-${RUN}`,
    notes: 'Primary smoke-test host in the blue zone',
    attributes: { cpu_cores: 16, memory_gb: 64, os_family: 'linux', os_version: 'Debian 13', management_ip: '10.77.1.10', purchase_date: '2025-03-01' },
  })).json;
  const database = (await post('/api/v1/configuration-items', {
    classId: dbClass, name: `smoke-db-${RUN}`, statusId: inService, attributes: { engine: 'postgresql', port: 5432, backup_enabled: true },
  })).json;
  const app = (await post('/api/v1/configuration-items', {
    classId: appClass, name: `smoke-app-${RUN}`, statusId: inService,
    attributes: { url: 'https://smoke.example.com', criticality: 'high', primary_database: database.id },
  })).json;
  check(app.attributeReferences?.primary_database?.name === database.name, 'reference attribute resolves to the database name');
  const listed = (await get(`/api/v1/configuration-items?classId=${appClass}&q=smoke-app-${RUN}`)).json;
  const listedApp = listed.data.find((c: Json) => c.id === app.id);
  check(listedApp?.attributes?.criticality === 'high' && listedApp?.attributes?.primary_database === database.id
    && listedApp?.attributeReferences?.primary_database?.name === database.name && !('memory_gb' in listedApp.attributes),
    'list items carry attribute values and reference names');
  await post('/api/v1/configuration-items', { classId: hardware, name: 'abstract', statusId: inService }, 400);
  await post('/api/v1/configuration-items', {
    classId: serverClass, name: 'bad', statusId: inService, hostname: '-bad-', ipAddress: '10.1.1.300',
  }, 400);
  await post('/api/v1/configuration-items', {
    classId: appClass, name: 'bad-attrs', statusId: inService, attributes: { url: 'ftp://x', criticality: 'extreme', primary_database: server.id, nope: 1 },
  }, 400);
  const bothStages = await post('/api/v1/configuration-items', {
    classId: serverClass, name: 'bad-both', statusId: inService, ipAddress: '999.1.1.1', attributes: { management_ip: 'abc', cpu_cores: 'x' },
  }, 400);
  const bothFields = (bothStages.json?.error?.details ?? []).map((d: Json) => d.field);
  check(['ipAddress', 'attributes.management_ip', 'attributes.cpu_cores'].every((f) => bothFields.includes(f)),
    'core-field and attribute errors are reported together');
  await post('/api/v1/configuration-items', { classId: lb.id, name: 'lb-missing-required', statusId: inService, attributes: { vip: '10.0.0.1' } }, 400);
  const lbItem = (await post('/api/v1/configuration-items', {
    classId: lb.id, name: `smoke-lb-${RUN}`, statusId: inService, attributes: { device_role: 'load_balancer', algorithm: 'round_robin', vip: '10.77.5.5', management_subnet: '10.77.5.0/24' },
  })).json;
  await post('/api/v1/configuration-items', { classId: lb.id, name: 'bad-cidr', statusId: inService, attributes: { device_role: 'load_balancer', algorithm: 'least_conn', management_subnet: '10.77.5.1/24' } }, 400); // host bits set
  const inUse = await patch(`/api/v1/attribute-definitions/${algo.id}`, { enumValues: ['least_conn'] }, 422); // round_robin in use
  check(inUse.json.error?.code === 'SCHEMA_CHANGE_REFUSED' && inUse.json.error.details?.[0]?.code === 'enum_value_in_use', 'an enum value still stored cannot be removed');
  check(lbItem.attributes.support === silver.id, 'a CI created without a value gets the attribute default');
  const lbUpd = (await patch(`/api/v1/configuration-items/${lbItem.id}`, { version: lbItem.version, attributes: { support: gold.id } })).json;
  check(lbUpd.attributes.support === gold.id, 'lookup value stored by id');
  await patch(`/api/v1/configuration-items/${lbItem.id}`, { version: lbUpd.version, attributes: { support: anyId } }, 400); // not in the list
  const slaRef = (await post('/api/v1/attribute-definitions', { classId: lb.id, key: 'sla_ref', label: 'SLA reference', dataType: 'text' })).json;
  const required = await patch(`/api/v1/attribute-definitions/${slaRef.id}`, { isRequired: true }, 422);
  check(required.json.error?.code === 'SCHEMA_CHANGE_REFUSED' && required.json.error.details?.[0]?.code === 'values_missing', 'an attribute cannot become required (NOT NULL) while CIs lack a value');

  await get(`/api/v1/configuration-items/${server.id}`);
  const upd = (await patch(`/api/v1/configuration-items/${server.id}`, { version: server.version, notes: 'Updated by smoke test', attributes: { memory_gb: 128, os_version: null } })).json;
  check(upd.version === server.version + 1 && upd.attributes.memory_gb === 128 && !('os_version' in upd.attributes), 'patch merges attributes, clears nulls, bumps version');
  await patch(`/api/v1/configuration-items/${server.id}`, { version: server.version, notes: 'stale' }, 409);
  await patch(`/api/v1/configuration-items/${server.id}`, { classId: dbClass }, 400); // server attributes not on database
  await patch(`/api/v1/configuration-items/${server.id}`, { attributes: { cpu_cores: 1.5 } }, 400);
  await patch(`/api/v1/configuration-items/${server.id}`, { version: 5 }, 400); // nothing to update

  // --- Relationships ------------------------------------------------------------
  console.log('\n# Relationships');
  const r1 = (await post('/api/v1/relationships', { relationshipTypeId: runsOn, sourceCiId: app.id, targetCiId: server.id })).json;
  const r2 = (await post('/api/v1/relationships', { relationshipTypeId: dependsOn, sourceCiId: app.id, targetCiId: database.id, notes: 'primary DB' })).json;
  await post('/api/v1/relationships', { relationshipTypeId: runsOn, sourceCiId: database.id, targetCiId: server.id });
  await post('/api/v1/relationships', { relationshipTypeId: runsOn, sourceCiId: app.id, targetCiId: server.id }, 409); // duplicate
  await post('/api/v1/relationships', { relationshipTypeId: runsOn, sourceCiId: app.id, targetCiId: app.id }, 400); // self edge
  await post('/api/v1/relationships', { relationshipTypeId: locatedIn, sourceCiId: database.id, targetCiId: server.id }, 400); // rule
  await post('/api/v1/relationships', { relationshipTypeId: runsOn, sourceCiId: app.id, targetCiId: '00000000-0000-4000-8000-000000000000' }, 400);
  await get(`/api/v1/relationships?ciId=${app.id}&sort=typeName`);
  await get(`/api/v1/relationships?q=smoke-app-${RUN}`);
  await get(`/api/v1/relationships/${r1.id}`);
  await patch(`/api/v1/relationships/${r2.id}`, { notes: 'primary DB (read/write)' });
  await patch(`/api/v1/relationships/${r2.id}`, { sourceCiId: server.id }, 400); // endpoints immutable

  // --- Graph --------------------------------------------------------------------
  console.log('\n# Graph');
  const g1 = (await get(`/api/v1/configuration-items/${app.id}/graph?direction=outgoing&depth=1`)).json;
  const names = (g: Json) => g.nodes.map((n: Json) => n.name);
  check(names(g1).includes(server.name) && names(g1).includes(database.name), 'app graph has server and database');
  const g2 = (await get(`/api/v1/configuration-items/${server.id}/graph?depth=2`)).json;
  check(
    [server.name, app.name, database.name].every((n) => names(g2).includes(n)) &&
      g2.edges.some((e: Json) => e.type.key === 'runs_on' && e.sourceCiId === app.id && e.targetCiId === server.id) &&
      g2.edges.some((e: Json) => e.type.key === 'depends_on' && e.sourceCiId === app.id && e.targetCiId === database.id),
    'Server -> Application -> Database in one graph call',
  );
  await get(`/api/v1/configuration-items/${server.id}/graph?depth=9`, 400);

  // --- Search -------------------------------------------------------------------
  console.log('\n# Search');
  const s1 = (await get(`/api/v1/search?q=smoke-srv-${RUN}.example`)).json;
  check(s1.data[0]?.item.id === server.id && s1.data[0].matches.some((m: Json) => m.field === 'hostname'), 'search by hostname');
  const s2 = (await get('/api/v1/search?q=10.77.0.10')).json;
  check(s2.data.some((r: Json) => r.item.id === server.id), 'search by exact IP');
  const s3 = (await get('/api/v1/search?q=10.77.0.0/16')).json;
  check(s3.data.some((r: Json) => r.item.id === server.id), 'search by CIDR containment');
  const s4 = (await get('/api/v1/search?q=smoke.example.com')).json;
  check(s4.data.some((r: Json) => r.item.id === app.id && r.matches.some((m: Json) => m.field === 'attributes.url')), 'search by attribute value');
  const s5 = (await get('/api/v1/search?q=test%20updated')).json; // words of the patched notes, any order
  check(s5.data.some((r: Json) => r.item.id === server.id && r.matches.some((m: Json) => m.field === 'notes')), 'search by notes words');
  await get(`/api/v1/search?q=SN-SMOKE-${RUN}&classId=${serverClass}`);
  await get(`/api/v1/configuration-items?q=smoke-&ownerId=${owner.id}`);
  await get('/api/v1/search', 400);

  await permissions({ serverClass, appClass, dbClass, server, app, database, r1, inService, adminMe });
  await customization({ serverClass, adminMe });
  await realTables({ inService, infra, adminMe });

  // --- Deletes and history ------------------------------------------------------
  console.log('\n# Deletes, audit');
  await del(`/api/v1/relationships/${r2.id}`);
  await del(`/api/v1/relationships/${r2.id}`, 404);
  await patch(`/api/v1/relationships/${r2.id}`, { notes: 'x' }, 409);
  await post('/api/v1/relationships', { relationshipTypeId: dependsOn, sourceCiId: app.id, targetCiId: database.id, notes: 're-created' });
  await del(`/api/v1/configuration-items/${lbItem.id}`);
  await del(`/api/v1/configuration-items/${server.id}`);
  const gone = (await get(`/api/v1/configuration-items/${server.id}`)).json;
  check(gone.deletedAt !== null, 'deleted CI is still readable with deletedAt');
  const liveEdges = (await get(`/api/v1/relationships?ciId=${server.id}`)).json;
  const allEdges = (await get(`/api/v1/relationships?ciId=${server.id}&deleted=include`)).json;
  check(liveEdges.page.total === 0 && allEdges.page.total >= 2, 'deleting a CI soft-deletes its relationships');
  await get(`/api/v1/configuration-items?deleted=only&q=smoke-srv-${RUN}`);
  await patch(`/api/v1/configuration-items/${server.id}`, { notes: 'x' }, 409);
  await del(`/api/v1/configuration-items/${server.id}`, 404);
  await post('/api/v1/relationships', { relationshipTypeId: runsOn, sourceCiId: app.id, targetCiId: server.id }, 400); // deleted endpoint
  await del(`/api/v1/owners/${owner.id}`, 409); // still owns the (deleted) server
  await patch(`/api/v1/owners/${owner.id}`, { isActive: false });
  await get(`/api/v1/relationship-rules/${rule.id}/usage`);
  await del(`/api/v1/relationship-rules/${rule.id}`);
  const rtUsage = (await get(`/api/v1/relationship-types/${rt.id}/usage`)).json;
  check(!rtUsage.inUse, 'an unused relationship type is not in use');
  await del(`/api/v1/relationship-types/${rt.id}`);
  const runsOnUsage = (await get(`/api/v1/relationship-types/${runsOn}/usage`)).json;
  check(runsOnUsage.inUse && runsOnUsage.data.some((u: Json) => u.kind === 'relationships' && u.count > 0), 'runs_on usage counts live relationships');
  const vipUsage = (await get(`/api/v1/attribute-definitions/${vip.id}/usage`)).json;
  check(vipUsage.data[0].kind === 'attributeValues' && vipUsage.data[0].count === 1, 'field usage counts stored values');
  await del(`/api/v1/attribute-definitions/${vip.id}`); // archives: the column and its value stay
  check((await get(`/api/v1/attribute-definitions/${vip.id}`)).json.isActive === false, 'deleting a field archives it');
  check((await get(`/api/v1/configuration-items/${lbItem.id}`)).json.attributes.vip === '10.77.5.5', 'an archived field keeps its stored values');
  await del(`/api/v1/attribute-definitions/${slaRef.id}`);
  const lbUsage = (await get(`/api/v1/ci-classes/${lb.id}/usage`)).json;
  check(lbUsage.inUse && lbUsage.data.some((u: Json) => u.kind === 'deletedConfigurationItems' && u.count === 1), 'class usage counts deleted CIs');
  await del(`/api/v1/ci-classes/${lb.id}`); // archives the type; its table and CIs stay
  check((await get(`/api/v1/ci-classes/${lb.id}`)).json.isActive === false, 'deleting a type archives it');
  await post('/api/v1/configuration-items', { classId: lb.id, name: 'archived-class', statusId: inService, attributes: { device_role: 'other', algorithm: 'least_conn' } }, 400);
  await get(`/api/v1/locations/${room.id}/usage`);
  await del(`/api/v1/locations/${room.id}`, 409);
  await get(`/api/v1/statuses/${inService}/usage`);
  await get(`/api/v1/environments/${production}/usage`);
  await get(`/api/v1/owners/${owner.id}/usage`);
  const silverUsage = (await get(`/api/v1/lookup-list-values/${silver.id}/usage`)).json;
  check(silverUsage.inUse && silverUsage.data.some((u: Json) => u.kind === 'attributeDefaults' && u.count === 1), 'a list value used as default is in use');
  await del(`/api/v1/lookup-list-values/${gold.id}`, 409); // stored on the (deleted) LB item
  await patch(`/api/v1/lookup-list-values/${gold.id}`, { isActive: false });
  await get(`/api/v1/lookup-lists/${contracts.id}/usage`);
  await del(`/api/v1/lookup-lists/${contracts.id}`, 409);
  await patch(`/api/v1/lookup-lists/${contracts.id}`, { isActive: false });
  const scratch = (await post('/api/v1/lookup-lists', { key: `smoke_scratch_${RUN}`, name: 'Scratch' })).json;
  await post('/api/v1/lookup-list-values', { listId: scratch.id, key: 'a', name: 'A' });
  await del(`/api/v1/lookup-lists/${scratch.id}`); // unused list goes with its values

  const history = (await get(`/api/v1/audit-log?entityType=configuration_items&entityId=${server.id}&sort=occurredAt`)).json;
  check(
    history.data.map((e: Json) => e.action).join(',') === 'create,update,delete' &&
      history.data[1].oldValue.attributes.memory_gb === 64 && history.data[1].newValue.attributes.memory_gb === 128 &&
      history.data.every((e: Json) => e.actorType === 'user' && e.actorId === adminMe.user.id && e.actorName === ADMIN_USERNAME && e.requestId),
    'audit log has create/update/delete with old/new values, the signed-in user and request id',
  );
  await get(`/api/v1/audit-log?actorId=${adminMe.user.id}&action=delete&limit=5`);
  await get('/api/v1/audit-log?actorName=smoke&action=delete&limit=5');
  await get('/api/v1/audit-log?from=not-a-date', 400);

  // --- Sign-out ------------------------------------------------------------------
  console.log('\n# Sign-out');
  await call('POST', '/api/v1/auth/logout', undefined, 403, { 'x-csrf-token': 'wrong' });
  const out = await post('/api/v1/auth/logout', undefined, 204);
  check(out.headers.getSetCookie().some((c) => c.startsWith('shadoucmdb_session=;') && c.includes('Max-Age=0')), 'logout clears the session cookie');
  await get('/api/v1/auth/me', 401);
  me = await login(ADMIN_USERNAME, ADMIN_PASSWORD);

  // --- HTTP-level errors ---------------------------------------------------------
  console.log('\n# HTTP errors');
  await call('POST', '/api/v1/statuses', '{"key":', 400);
  await call('POST', '/api/v1/statuses', 'key=x', 415, { 'content-type': 'application/x-www-form-urlencoded' });
  await get('/api/v1/does-not-exist', 404);
  await get('/api/v1/configuration-items?limit=500', 400);

  // --- Coverage -------------------------------------------------------------------
  const missing = ops.filter((o) => !covered.has(o.operationId));
  console.log(`\n# ${calls} requests, ${covered.size}/${ops.length} OpenAPI operations exercised, ${checkedBodies} response bodies checked against their schemas`);
  for (const m of missing) failures.push(`operation not exercised: ${m.method} ${m.path} (${m.operationId})`);
  if (failures.length) {
    console.log(`\n${failures.length} FAILURE(S):\n- ${failures.join('\n- ')}`);
    process.exit(1);
  }
  console.log('ALL CHECKS PASSED');
}

/** Profiles, users, class-scoped and global permissions, CSRF, backoff, lockout protection. */
async function permissions(x: Json) {
  const { serverClass, appClass, dbClass, server, app, database, r1, inService, adminMe } = x;
  const put = (url: string, body: unknown, expect = 200) => call('PUT', url, body, expect);
  const builtin = adminMe.user.profiles.find((p: Json) => p.isBuiltin);

  console.log('\n# Permission profiles');
  const profiles = (await get('/api/v1/admin/profiles?sort=name')).json;
  check(profiles.data[0]?.isBuiltin && profiles.data[0].id === builtin.id, 'the built-in Administrator profile is listed first');
  await get(`/api/v1/admin/profiles/${builtin.id}`);
  await patch(`/api/v1/admin/profiles/${builtin.id}`, { name: 'Renamed' }, 409);
  await del(`/api/v1/admin/profiles/${builtin.id}`, 409);
  const readers = (await post('/api/v1/admin/profiles', {
    name: `smoke-readers-${RUN}`,
    classPermissions: [{ classId: serverClass, view: true, create: false, edit: false, delete: false }],
  })).json;
  await post('/api/v1/admin/profiles', { name: `SMOKE-READERS-${RUN}` }, 409); // names are unique regardless of case
  await post('/api/v1/admin/profiles', { name: 'bad', globalPermissions: ['users.delete'] }, 400);
  await post('/api/v1/admin/profiles', { name: 'bad', classPermissions: [{ classId: serverClass, view: true, create: false, edit: false, delete: false }, { classId: serverClass, view: true, create: true, edit: false, delete: false }] }, 400);
  await post('/api/v1/admin/profiles', { name: 'bad', classPermissions: [{ classId: '00000000-0000-4000-8000-000000000000', view: true, create: false, edit: false, delete: false }] }, 400);
  const editors = (await post('/api/v1/admin/profiles', {
    name: `smoke-editors-${RUN}`,
    description: 'Edit servers',
    globalPermissions: ['audit.view'],
    classPermissions: [{ classId: serverClass, view: false, create: true, edit: true, delete: false }],
  })).json;
  check(editors.classPermissions[0]?.view === true, 'write rights imply view');
  await patch(`/api/v1/admin/profiles/${editors.id}`, { description: null, globalPermissions: [] });
  const copy = (await post(`/api/v1/admin/profiles/${builtin.id}/clone`, { name: `smoke-admin-copy-${RUN}` })).json;
  check(!copy.isBuiltin && copy.globalPermissions.length === 6 && copy.classPermissions[0]?.classId === null, 'cloning Administrator gives an editable profile with every permission');
  await post(`/api/v1/admin/profiles/${readers.id}/clone`, { name: `smoke-readers-${RUN}` }, 409);
  await get(`/api/v1/admin/profiles?q=smoke-&limit=5`);

  console.log('\n# Users');
  const password = `reader-${RUN}-password`;
  const reader = (await post('/api/v1/admin/users', { username: `smoke-reader-${RUN}`, displayName: 'Smoke reader', email: `reader-${RUN}@example.com`, password, profileIds: [readers.id] })).json;
  check(reader.profiles.length === 1 && !reader.isAdministrator && !('passwordHash' in reader), 'user has the profile and no password hash in the API');
  await post('/api/v1/admin/users', { username: `SMOKE-READER-${RUN}`, displayName: 'dup', password }, 409);
  await post('/api/v1/admin/users', { username: 'has space', displayName: 'x', password: 'short' }, 400);
  await post('/api/v1/admin/users', { username: `x-${RUN}`, displayName: 'x', password, profileIds: ['00000000-0000-4000-8000-000000000000'] }, 400);
  await get(`/api/v1/admin/users?q=smoke-reader-${RUN}&isActive=true&profileId=${readers.id}&sort=-createdAt`);
  await get(`/api/v1/admin/users/${reader.id}`);
  await get('/api/v1/admin/users/00000000-0000-4000-8000-000000000000', 404);
  const nobody = (await post('/api/v1/admin/users', { username: `smoke-nobody-${RUN}`, displayName: 'No permissions', password })).json;

  console.log('\n# Class permissions (reader: view servers only)');
  const asReader = await login(reader.username.toUpperCase(), password); // usernames are case-insensitive
  await as(asReader, async () => {
    const mine = (await get('/api/v1/auth/me')).json;
    check(!mine.permissions.administrator && mine.permissions.classes.length === 1 && mine.permissions.classes[0].classId === serverClass && mine.permissions.global.length === 0, 'reader sees their effective permissions');
    const list = (await get('/api/v1/configuration-items?limit=200')).json;
    check(list.data.length > 0 && list.data.every((c: Json) => c.classId === serverClass), 'inventory only lists classes the user may view');
    await get(`/api/v1/configuration-items/${server.id}`);
    await get(`/api/v1/configuration-items/${app.id}`, 403);
    await post('/api/v1/configuration-items', { classId: serverClass, name: 'nope', statusId: inService }, 403);
    await patch(`/api/v1/configuration-items/${server.id}`, { notes: 'nope' }, 403);
    await del(`/api/v1/configuration-items/${server.id}`, 403);
    const g = (await get(`/api/v1/configuration-items/${server.id}/graph?depth=2`)).json;
    check(g.nodes.every((n: Json) => n.classId === serverClass) && g.edges.length === 0, 'graph leaves out classes the user may not view');
    await get(`/api/v1/configuration-items/${app.id}/graph`, 403);
    const found = (await get(`/api/v1/search?q=smoke-`)).json;
    check(found.data.every((h: Json) => h.item.classId === serverClass), 'search only returns classes the user may view');
    const edges = (await get(`/api/v1/relationships?ciId=${app.id}`)).json;
    check(edges.page.total === 0, 'relationships to hidden classes are not listed');
    await get(`/api/v1/relationships/${r1.id}`, 403);
    await post('/api/v1/relationships', { relationshipTypeId: r1.relationshipTypeId, sourceCiId: app.id, targetCiId: database.id }, 403);
    await get('/api/v1/statuses?limit=5'); // the data model and lookups are readable
    await get(`/api/v1/ci-classes/${serverClass}/attributes`);
    await post('/api/v1/statuses', { key: `nope_${RUN}`, name: 'Nope' }, 403);
    await get('/api/v1/audit-log', 403);
    await get('/api/v1/admin/users', 403);
    await get('/api/v1/admin/profiles', 403);
    await call('POST', '/api/v1/statuses', { key: `nope_${RUN}`, name: 'Nope' }, 403, { 'x-csrf-token': '' }).then((r) =>
      check(r.json.error?.code === 'CSRF_TOKEN_INVALID', 'a write without the CSRF token is rejected before anything else'));
    await put('/api/v1/auth/password', { currentPassword: 'wrong password!', newPassword: `${password}-2` }, 400);
    await put('/api/v1/auth/password', { currentPassword: password, newPassword: `${password}-2` }, 204);
    await get('/api/v1/auth/me'); // this session survives the change
  });
  await loginFails(reader.username, password); // the old password no longer works

  console.log('\n# Reference attributes into classes the caller may not view (app editor, no database rights)');
  const appEditors = (await post('/api/v1/admin/profiles', {
    name: `smoke-app-editors-${RUN}`,
    classPermissions: [{ classId: appClass, view: true, create: true, edit: true, delete: false }],
  })).json;
  const appEditor = (await post('/api/v1/admin/users', { username: `smoke-app-editor-${RUN}`, displayName: 'App editor', password, profileIds: [appEditors.id] })).json;
  const otherDb = (await post('/api/v1/configuration-items', { classId: dbClass, name: `smoke-db-hidden-${RUN}`, statusId: inService, attributes: { engine: 'postgresql' } })).json;
  const hiddenRef = (ci: Json) => {
    const r = ci.attributeReferences?.primary_database;
    return ci.attributes?.primary_database === database.id && r?.id === database.id && r.hidden === true && r.name === null && r.deleted === false;
  };
  await as(await login(appEditor.username, password), async () => {
    const shown = (await get(`/api/v1/configuration-items/${app.id}`)).json;
    check(hiddenRef(shown) && !JSON.stringify(shown).includes(database.name), 'detail: a reference into a hidden class has no name (hidden: true)');
    const listed = (await get(`/api/v1/configuration-items?classId=${appClass}&q=smoke-app-${RUN}`)).json.data.find((c: Json) => c.id === app.id);
    check(hiddenRef(listed) && !JSON.stringify(listed).includes(database.name), 'list: a reference into a hidden class has no name');
    const kept = (await patch(`/api/v1/configuration-items/${app.id}`, { attributes: { primary_database: database.id } })).json;
    check(hiddenRef(kept), 'resending the unchanged hidden reference is accepted and stays hidden');
    const code = (r: { json: Json }) => JSON.stringify([r.json.error?.code, r.json.error?.details]);
    const existing = await patch(`/api/v1/configuration-items/${app.id}`, { attributes: { primary_database: otherDb.id } }, 400);
    const missing = await patch(`/api/v1/configuration-items/${app.id}`, { attributes: { primary_database: '00000000-0000-4000-8000-000000000000' } }, 400);
    check(code(existing) === code(missing) && existing.json.error?.details?.[0]?.code === 'not_found' && !JSON.stringify(existing.json).includes(otherDb.name),
      'setting a reference to a CI in a hidden class fails exactly like a missing one (no existence oracle)');
    // GH#45: a body that fails its schema still gets its attributes checked, with the same reference access.
    const refFailed = (r: { json: Json }) => r.json.error?.details?.some((d: Json) => d.field === 'attributes.primary_database' && d.code === 'not_found');
    const create = (ref: string) => post('/api/v1/configuration-items', {
      classId: appClass, name: `smoke-app-bad-${RUN}`, statusId: inService, ipAddress: '999.1.1.1', attributes: { primary_database: ref },
    }, 400);
    const createExisting = await create(otherDb.id);
    const createMissing = await create('00000000-0000-4000-8000-000000000000');
    check(code(createExisting) === code(createMissing) && fields(createExisting).includes('ipAddress') && refFailed(createExisting) &&
      !JSON.stringify(createExisting.json).includes(otherDb.name),
      'create with an invalid body: a reference into a hidden class fails exactly like a missing one');
    const update = (ref: string) => patch(`/api/v1/configuration-items/${app.id}`, { ipAddress: '999.1.1.1', attributes: { primary_database: ref } }, 400);
    const updateExisting = await update(otherDb.id);
    const updateMissing = await update('00000000-0000-4000-8000-000000000000');
    check(code(updateExisting) === code(updateMissing) && fields(updateExisting).includes('ipAddress') && refFailed(updateExisting) &&
      !JSON.stringify(updateExisting.json).includes(otherDb.name),
      'update with an invalid body: a reference into a hidden class fails exactly like a missing one');
    const unchanged = await update(database.id);
    check(JSON.stringify(fields(unchanged)) === '["ipAddress"]', 'update with an invalid body: the unchanged hidden reference is not flagged');
  });
  const asAdmin = (await get(`/api/v1/configuration-items/${app.id}`)).json.attributeReferences?.primary_database;
  check(asAdmin?.name === database.name && asAdmin.hidden === false, 'an administrator still sees the referenced name');
  await del(`/api/v1/configuration-items/${otherDb.id}`, 204);
  await del(`/api/v1/admin/users/${appEditor.id}`);
  await del(`/api/v1/admin/profiles/${appEditors.id}`);

  console.log('\n# Global permissions (no profiles: 403 on every permission-guarded operation)');
  const asNobody = await login(nobody.username, password);
  await as(asNobody, async () => {
    let guarded = 0;
    for (const op of ops.filter((o) => /Requires `/.test(o.description))) {
      const url = op.path.replace(/\{[^}]+\}/g, '00000000-0000-4000-8000-000000000000');
      const res = await call(op.method, url, op.method === 'GET' || op.method === 'DELETE' ? undefined : {}, 403, {}, { cover: false });
      if (res.json?.error?.code !== 'FORBIDDEN') failures.push(`${op.operationId}: expected FORBIDDEN without the permission`);
      guarded++;
    }
    check(guarded >= 20, `every permission-guarded operation answers 403 (${guarded} checked)`);
    check((await get('/api/v1/configuration-items')).json.page.total === 0, 'no class permissions: an empty inventory');
  });

  console.log('\n# Escalation guards (users.manage without other permissions)');
  const userManagers = (await post('/api/v1/admin/profiles', { name: `smoke-user-managers-${RUN}`, globalPermissions: ['users.manage'] })).json;
  await patch(`/api/v1/admin/users/${nobody.id}`, { profileIds: [userManagers.id] });
  await as(asNobody, async () => {
    await post('/api/v1/admin/users', { username: `smoke-escalate-${RUN}`, displayName: 'x', password, profileIds: [builtin.id] }, 403);
    await post('/api/v1/admin/users', { username: `smoke-escalate-${RUN}`, displayName: 'x', password, profileIds: [readers.id] }, 403);
    await patch(`/api/v1/admin/users/${adminMe.user.id}`, { displayName: 'pwned' }, 403);
    await put(`/api/v1/admin/users/${adminMe.user.id}/password`, { password: 'correct horse battery' }, 403);
    await patch(`/api/v1/admin/users/${reader.id}`, { profileIds: [] }, 403); // reader can view servers, the manager cannot
    const plain = (await post('/api/v1/admin/users', { username: `smoke-plain-${RUN}`, displayName: 'Plain', password })).json;
    await del(`/api/v1/admin/users/${plain.id}`);
    await del(`/api/v1/admin/users/${nobody.id}`, 409); // not yourself
  });

  console.log('\n# Lockout protection');
  await patch(`/api/v1/admin/users/${adminMe.user.id}`, { isActive: false }, 409);
  await del(`/api/v1/admin/users/${adminMe.user.id}`, 409);
  const activeAdmins = (await get(`/api/v1/admin/users?profileId=${builtin.id}&isActive=true`)).json.page.total;
  if (activeAdmins === 1) {
    const last = await patch(`/api/v1/admin/users/${adminMe.user.id}`, { profileIds: [] }, 409);
    check(last.json.error?.code === 'LAST_ADMINISTRATOR', 'the last active Administrator cannot lose the profile');
  }
  const second = (await post('/api/v1/admin/users', { username: `smoke-admin2-${RUN}`, displayName: 'Second admin', password, profileIds: [builtin.id] })).json;
  await patch(`/api/v1/admin/users/${second.id}`, { profileIds: [editors.id] }); // fine: another administrator remains
  await del(`/api/v1/admin/users/${second.id}`);

  console.log('\n# Disable, reset password');
  const readerSession = await login(reader.username, `${password}-2`);
  const disabled = (await patch(`/api/v1/admin/users/${reader.id}`, { isActive: false })).json;
  check(disabled.isActive === false, 'user disabled');
  await as(readerSession, () => get('/api/v1/auth/me', 401)); // disabling ends their sessions
  await loginFails(reader.username, `${password}-2`);
  await patch(`/api/v1/admin/users/${reader.id}`, { isActive: true, displayName: 'Smoke reader (back)', email: null });
  const reset = (await put(`/api/v1/admin/users/${reader.id}/password`, { password: `${password}-3` })).json;
  check(reset.passwordChangedAt > reader.passwordChangedAt, 'password reset recorded');
  await put(`/api/v1/admin/users/${reader.id}/password`, { password: 'short' }, 400);
  await login(reader.username, `${password}-3`);

  console.log('\n# Login backoff');
  const ghost = `smoke-ghost-${RUN}`;
  for (let i = 0; i < 4; i++) await loginFails(ghost, 'wrong password');
  // The 5th failure locks the name for 1 s, the 6th for 2 s, ...; a slow (debug) server may outlast the first lock.
  let limited: Json = null;
  for (let i = 0; i < 8 && !limited; i++) {
    const res = await as(null, () => call('POST', '/api/v1/auth/login', { username: ghost, password: 'wrong password' }, undefined, {}, { accept: [401, 429] }));
    if (res?.status === 429) limited = res;
  }
  check(limited?.json.error?.code === 'RATE_LIMITED' && Number(limited.headers.get('retry-after')) >= 1, 'repeated failures lock the username (429 with Retry-After)');
  await as(null, () => post('/api/v1/auth/login', { username: ADMIN_USERNAME, password: '' }, 400));

  console.log('\n# Audit of administration');
  const trail = (await get(`/api/v1/audit-log?entityType=users&entityId=${reader.id}&sort=occurredAt`)).json;
  check(trail.data.length >= 5 && trail.data[0].action === 'create' && trail.data.every((e: Json) => e.actorType === 'user' && typeof e.actorId === 'string') &&
    trail.data.some((e: Json) => e.actorId === reader.id), 'user changes are audited with the acting user (including self-service)');
  check(trail.data.every((e: Json) => !JSON.stringify(e).includes('argon2')), 'password hashes never reach the audit log');
  await get(`/api/v1/audit-log?entityType=permission_profiles&entityId=${readers.id}`);

  console.log('\n# Audit of authentication');
  // Through a proxy that sets X-Forwarded-For: its first hop is the client IP.
  const viaProxy = { 'x-forwarded-for': '203.0.113.38, 10.0.0.1' };
  const loginVia = (username: string, pw: string, expect: number) =>
    as(null, () => call('POST', '/api/v1/auth/login', { username, password: pw }, expect, viaProxy));
  await loginVia(reader.username, 'wrong password', 401);
  const stranger = `smoke-stranger-${RUN}`;
  await loginVia(stranger, 'wrong password', 401); // same headers as the reader's failure, but no such user
  const audited = identityFrom(reader.username, await loginVia(reader.username, `${password}-3`, 200));
  await as(audited, () => call('POST', '/api/v1/auth/logout', undefined, 204, viaProxy));
  const revokedByReset = identityFrom(reader.username, await loginVia(reader.username, `${password}-3`, 200));
  await put(`/api/v1/admin/users/${reader.id}/password`, { password: `${password}-4` });
  const events: Json[] = (await get('/api/v1/audit-log?entityType=sessions&limit=200')).json.data;
  await get('/api/v1/audit-log?entityType=sessions&action=login.failure&limit=5');
  const about = (e: Json, name: string) => [e.newValue?.username, e.newValue?.attemptedUsername].includes(name);
  const ofReader = events.filter((e) => about(e, reader.username));
  const success = ofReader.find((e) => e.action === 'login.success');
  check(success && success.actorType === 'user' && success.actorId === reader.id && success.newValue.userId === reader.id &&
    success.newValue.ipAddress === '203.0.113.38' && typeof success.newValue.userAgent === 'string' && success.oldValue === null &&
    typeof success.requestId === 'string', 'login.success: the user as actor, client IP from X-Forwarded-For, user agent, request id');
  const failure = ofReader.find((e) => e.action === 'login.failure');
  const ghostFailure = events.find((e) => e.action === 'login.failure' && about(e, ghost));
  const strangerFailure = events.find((e) => e.action === 'login.failure' && about(e, stranger));
  const keys = (e: Json) => Object.keys(e?.newValue ?? {}).sort().join(',');
  const { attemptedUsername: _a, ...readerRest } = failure?.newValue ?? {};
  const { attemptedUsername: _b, ...strangerRest } = strangerFailure?.newValue ?? {};
  check(failure && failure.actorId === null && failure.newValue.attemptedUsername === reader.username && failure.newValue.ipAddress === '203.0.113.38',
    'login.failure: no actor id, the attempted username and the client IP');
  check([success, failure].every((e) => typeof e?.newValue.peerIpAddress === 'string' && e.newValue.peerIpAddress !== '203.0.113.38') &&
    ghostFailure && ghostFailure.newValue.peerIpAddress === undefined,
    'the TCP peer is kept as peerIpAddress when X-Forwarded-For names another address, and only then');
  // Same request headers, existing vs unknown name: every field but the name itself must match, peerIpAddress included.
  check(strangerFailure && keys(failure) === 'attemptedUsername,ipAddress,peerIpAddress,userAgent' && keys(strangerFailure) === keys(failure) &&
    JSON.stringify(readerRest) === JSON.stringify(strangerRest),
    'login.failure looks the same for existing and unknown usernames (no enumeration oracle)');
  const locked = events.find((e) => e.action === 'login.locked' && about(e, ghost));
  check(locked && locked.actorId === null && locked.newValue.lockedForSeconds >= 1, 'login.locked: the lock and its duration');
  check(ofReader.some((e) => e.action === 'logout' && e.actorId === reader.id && e.newValue.ipAddress === '203.0.113.38' &&
    e.newValue.session?.ipAddress === '203.0.113.38'), 'logout: the user as actor, with the session\'s IP');
  const revoked = (reason: string) => ofReader.filter((e) => e.action === 'session.revoke' && e.newValue.reason === reason);
  check(revoked('user_disabled').some((e) => e.actorId === adminMe.user.id && e.newValue.userId === reader.id), 'disabling a user writes session.revoke (actor: the administrator)');
  check(revoked('password_reset').some((e) => e.actorId === adminMe.user.id && e.newValue.session?.ipAddress === '203.0.113.38'), 'an admin password reset writes session.revoke');
  // Nothing secret: passwords, hashes, session tokens (or their SHA-256), CSRF tokens.
  const { createHash } = await import('node:crypto');
  const secrets = [audited, revokedByReset, me!].flatMap((s) => {
    const token = s.cookie.split('=')[1] ?? '';
    return [token, createHash('sha256').update(token).digest('hex'), s.csrf];
  }).concat([password, `${password}-2`, `${password}-3`, `${password}-4`, 'wrong password', 'argon2']);
  check(events.every((e) => secrets.every((s) => s && !JSON.stringify(e).includes(s))), 'no password, hash, session token or CSRF token in the authentication audit rows');

  console.log('\n# API tokens');
  const inDays = (d: number) => new Date(Date.now() + d * 86_400_000).toISOString();
  const tokens = '/api/v1/admin/api-tokens';
  // The reader's token, scoped to Administrator: it still only gets what the reader holds (view servers).
  const readerToken = (await post(tokens, { name: `smoke-token-${RUN}`, userId: reader.id, profileId: builtin.id, expiresAt: inDays(30) })).json;
  check(/^scmdb_[0-9a-f]{64}$/.test(readerToken.secret) && readerToken.token.tokenPrefix === readerToken.secret.slice(0, 14) &&
    readerToken.token.status === 'active' && readerToken.token.userId === reader.id, 'a new token: the secret, its prefix, active, owned by the reader');
  await post(tokens, { name: 'x', profileId: readers.id, expiresAt: inDays(-1) }, 400);
  await post(tokens, { name: 'x', profileId: readers.id, expiresAt: inDays(400) }, 400);
  await post(tokens, { name: 'x', profileId: '00000000-0000-4000-8000-000000000000', expiresAt: inDays(1) }, 400);
  const listed = (await get(`${tokens}?userId=${reader.id}&status=active&q=smoke-token&sort=-createdAt`)).json;
  check(listed.page.total === 1 && !JSON.stringify(listed).includes(readerToken.secret), 'tokens are listed, never with their secret');
  await get(`${tokens}/${readerToken.token.id}`);
  await get(`${tokens}/00000000-0000-4000-8000-000000000000`, 404);
  const readerBearer = { authorization: `Bearer ${readerToken.secret}` };
  await as(null, async () => {
    const list = (await call('GET', '/api/v1/configuration-items?limit=200', undefined, 200, readerBearer)).json;
    check(list.data.length > 0 && list.data.every((c: Json) => c.classId === serverClass), 'a token sees what its owner may see, not what its scope alone allows');
    await call('GET', `/api/v1/configuration-items/${app.id}`, undefined, 403, readerBearer);
    await call('GET', '/api/v1/admin/users', undefined, 403, readerBearer);
    await call('GET', tokens, undefined, 403, readerBearer, { cover: false }); // token administration needs a session
    await call('GET', '/api/v1/auth/me', undefined, 403, readerBearer);
    await call('GET', '/api/v1/statuses?limit=1', undefined, 401, { authorization: 'Bearer scmdb_not-a-token' });
  });
  // A bad Bearer next to a live session is 401 (the cookie is ignored), not a way around the CSRF check.
  await call('POST', '/api/v1/statuses', { key: `nope_${RUN}`, name: 'Nope' }, 401, { 'x-csrf-token': '', authorization: 'Bearer scmdb_x' });
  // The administrator's token scoped to editors (create/edit servers): writes without a CSRF token, audited as api_client.
  const editorToken = (await post(tokens, { name: `smoke-editor-token-${RUN}`, profileId: editors.id, expiresAt: inDays(1) })).json;
  const editorBearer = { authorization: `Bearer ${editorToken.secret}` };
  const made = await as(null, () => call('POST', '/api/v1/configuration-items', { classId: serverClass, name: `smoke-token-srv-${RUN}`, statusId: inService }, 201, editorBearer));
  await as(null, () => call('PATCH', `/api/v1/configuration-items/${made.json.id}`, { notes: 'edited by token' }, 200, editorBearer));
  await as(null, () => call('DELETE', `/api/v1/configuration-items/${made.json.id}`, undefined, 403, editorBearer));
  await del(`/api/v1/configuration-items/${made.json.id}`);
  const requestId = made.headers.get('x-request-id');
  const byToken = (await get(`/api/v1/audit-log?requestId=${requestId}`)).json.data;
  check(byToken.some((e: Json) => e.action === 'create' && e.entityType === 'configuration_items' && e.actorType === 'api_client' && e.actorId === adminMe.user.id) &&
    byToken.some((e: Json) => e.action === 'token.use' && e.entityId === editorToken.token.id && e.newValue.outcome === 'accepted' && e.newValue.method === 'POST'),
    'a change made with a token: actor api_client (the owner), and a token.use row with the same request id');
  // Escalation: a user manager without other rights cannot mint tokens for users who hold more.
  await as(asNobody, async () => {
    await post(tokens, { name: 'x', userId: adminMe.user.id, profileId: readers.id, expiresAt: inDays(1) }, 403);
    await post(tokens, { name: 'x', userId: reader.id, profileId: readers.id, expiresAt: inDays(1) }, 403);
    await del(`${tokens}/${readerToken.token.id}`, 403);
    await post(tokens, { name: `smoke-own-${RUN}`, profileId: readers.id, expiresAt: inDays(1) }); // their own is fine
  });
  // Revoke: at once, idempotent, kept as history.
  await del(`${tokens}/${readerToken.token.id}`);
  await del(`${tokens}/${readerToken.token.id}`);
  await del(`${tokens}/${editorToken.token.id}`);
  await as(null, () => call('GET', '/api/v1/configuration-items?limit=1', undefined, 401, readerBearer));
  const gone = (await get(`${tokens}/${readerToken.token.id}`)).json;
  check(gone.status === 'revoked' && gone.revokedBy === ADMIN_USERNAME && gone.lastUsedAt, 'a revoked token stays listed with who revoked it and when it was last used');
  const tokenTrail: Json[] = (await get(`/api/v1/audit-log?entityType=api_tokens&entityId=${readerToken.token.id}&sort=occurredAt&limit=50`)).json.data;
  const outcomes = tokenTrail.map((e) => (e.action === 'token.use' ? `use:${e.newValue.outcome}` : e.action)).join(',');
  check(outcomes === 'create,use:accepted,use:accepted,use:forbidden,use:session_only,use:session_only,update,use:revoked', `token create, every use and revoke are audited (${outcomes})`);
  check(tokenTrail.every((e) => !JSON.stringify(e).includes(readerToken.secret.slice(6)) &&
    !JSON.stringify(e).includes(createHash('sha256').update(readerToken.secret).digest('hex'))), 'no token secret or hash in the audit log');

  await mfa(builtin, createHash);

  // Clean up what only this run uses.
  await del(`/api/v1/admin/users/${reader.id}`);
  await del(`/api/v1/admin/users/${nobody.id}`);
  for (const p of [readers, editors, copy, userManagers]) await del(`/api/v1/admin/profiles/${p.id}`);
  await del(`/api/v1/admin/profiles/${readers.id}`, 404);
}

/** The code an authenticator app shows for a base32 secret at a 30-second step (RFC 6238, HMAC-SHA1, 6 digits). */
async function totpCode(secret: string, step: number): Promise<string> {
  const { createHmac } = await import('node:crypto');
  let buffer = 0, bits = 0;
  const key: number[] = [];
  for (const c of secret) {
    buffer = (buffer << 5) | 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'.indexOf(c);
    bits += 5;
    if (bits >= 8) {
      bits -= 8;
      key.push((buffer >> bits) & 0xff);
    }
  }
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(step));
  const d = createHmac('sha1', Buffer.from(key)).update(counter).digest();
  const offset = d[d.length - 1]! & 0x0f;
  return String((d.readUInt32BE(offset) & 0x7fffffff) % 1_000_000).padStart(6, '0');
}

/** TOTP enrolment, the second sign-in step, recovery codes, the admin reset and the per-profile requirement. */
async function mfa(builtin: Json, createHash: typeof import('node:crypto').createHash) {
  console.log('\n# Two-factor authentication');
  const nowStep = () => Math.floor(Date.now() / 30_000);
  const password = `mfa-${RUN}-password`;
  const user = (await post('/api/v1/admin/users', { username: `smoke-mfa-${RUN}`, displayName: 'Smoke MFA', password })).json;
  check(user.mfaEnabled === false, 'a new user has no MFA');
  const session = await login(user.username, password);
  const { secret, codes, lastStep } = await as(session, async () => {
    check((await get('/api/v1/auth/mfa')).json.totpEnabled === false, 'MFA status: off');
    await post('/api/v1/auth/mfa/totp', { currentPassword: 'wrong password!' }, 400);
    const started = (await post('/api/v1/auth/mfa/totp', { currentPassword: password })).json;
    check(/^[A-Z2-7]{32}$/.test(started.secret) && started.otpauthUri.startsWith('otpauth://totp/') && started.digits === 6 && started.period === 30,
      'set-up returns a 160-bit base32 secret and its otpauth URI');
    await post('/api/v1/auth/mfa/totp/confirm', { code: await totpCode(started.secret, nowStep() - 5) }, 400);
    const step = nowStep();
    const confirmed = (await post('/api/v1/auth/mfa/totp/confirm', { code: await totpCode(started.secret, step) }, 200)).json;
    check(confirmed.codes.length === 10 && confirmed.codes.every((c: string) => /^[a-z2-7]{4}(-[a-z2-7]{4}){3}$/.test(c)), 'confirming returns 10 recovery codes');
    await post('/api/v1/auth/mfa/totp/confirm', { code: await totpCode(started.secret, step + 1) }, 409);
    await post('/api/v1/auth/mfa/totp', { currentPassword: password }, 409);
    const status = (await get('/api/v1/auth/mfa')).json;
    check(status.totpEnabled && status.recoveryCodesRemaining === 10, 'MFA status: on, 10 recovery codes left');
    return { secret: started.secret as string, codes: confirmed.codes as string[], lastStep: step };
  });

  // Sign-in now takes two steps; the challenge cookie carries the first one.
  const firstStep = async () => {
    const res = await loginFails(user.username, password);
    check(res.json.error?.code === 'MFA_REQUIRED', 'a right password answers MFA_REQUIRED');
    const cookie = res.headers.getSetCookie().map((c) => c.split(';')[0]!).find((c) => c.startsWith('shadoucmdb_mfa='));
    check(cookie, 'the MFA challenge cookie is set');
    return { name: 'mfa challenge', cookie: cookie ?? '', csrf: '' };
  };
  await as(null, () => post('/api/v1/auth/login/mfa', { code: '123456' }, 401)); // no challenge
  const challenge = await firstStep();
  const stale = await totpCode(secret, lastStep - 5);
  await as(challenge, () => post('/api/v1/auth/login/mfa', { code: stale }, 401));
  await as(challenge, () => post('/api/v1/auth/login/mfa', { code: 'short' }, 400));
  const signedIn = identityFrom(user.username, await as(challenge, async () => post('/api/v1/auth/login/mfa', { code: await totpCode(secret, lastStep + 1) }, 200)));
  await as(challenge, () => post('/api/v1/auth/login/mfa', { code: codes[0]! }, 401)); // the challenge is used up
  const newCodes: string[] = await as(signedIn, async () => {
    await get('/api/v1/auth/me');
    await post('/api/v1/auth/mfa/recovery-codes', { currentPassword: password, code: '000000' }, 400);
    const fresh = (await post('/api/v1/auth/mfa/recovery-codes', { currentPassword: password, code: codes[0]!.toUpperCase() }, 200)).json.codes;
    await post('/api/v1/auth/mfa/recovery-codes', { currentPassword: password, code: codes[1]! }, 400); // the old codes are gone
    return fresh;
  });
  const recovered = identityFrom(user.username, await as(await firstStep(), () => post('/api/v1/auth/login/mfa', { code: newCodes[0]! }, 200)));
  await as(recovered, async () => {
    check((await get('/api/v1/auth/mfa')).json.recoveryCodesRemaining === 9, 'a used recovery code is gone');
  });

  // An administrator resets a lost authenticator; a profile can require MFA.
  check((await get(`/api/v1/admin/users/${user.id}`)).json.mfaEnabled === true, 'the user list shows mfaEnabled');
  await del('/api/v1/admin/users/00000000-0000-4000-8000-000000000000/mfa', 404);
  await del(`/api/v1/admin/users/${user.id}/mfa`);
  await patch(`/api/v1/admin/profiles/${builtin.id}`, { requireMfa: false }); // the one change the built-in profile allows
  const required = (await post('/api/v1/admin/profiles', { name: `smoke-mfa-required-${RUN}`, requireMfa: true })).json;
  check(required.requireMfa === true, 'a profile can require MFA');
  await patch(`/api/v1/admin/users/${user.id}`, { profileIds: [required.id] });
  const mustEnrol = await login(user.username, password); // the password alone signs in after the reset
  await as(mustEnrol, async () => {
    check((await get('/api/v1/auth/mfa')).json.enrolmentRequired === true, 'MFA status: set-up required');
    const blocked = await get('/api/v1/statuses?limit=1', 403);
    check(blocked.json.error?.code === 'MFA_ENROLMENT_REQUIRED', 'other routes answer MFA_ENROLMENT_REQUIRED until MFA is set up');
    await post('/api/v1/auth/mfa/totp', { currentPassword: password });
    await call('DELETE', '/api/v1/auth/mfa/totp', { currentPassword: password, code: '000000' }, 204); // cancels the unfinished set-up
    await call('DELETE', '/api/v1/auth/mfa/totp', { currentPassword: password, code: '000000' }, 409);
  });

  const trail: Json[] = (await get(`/api/v1/audit-log?entityType=users&entityId=${user.id}&sort=occurredAt&limit=100`)).json.data;
  const actions = trail.filter((e) => e.action.startsWith('mfa.')).map((e) => e.action).join(',');
  check(actions === 'mfa.enrol,mfa.failure,mfa.failure,mfa.recovery_code_used,mfa.recovery_codes,mfa.failure,mfa.recovery_code_used,mfa.disable',
    `enrolment, failures, recovery codes and the reset are audited (${actions})`);
  const secrets = [secret, ...codes, ...newCodes].flatMap((s) => [s, s.replaceAll('-', '')]);
  check(trail.every((e) => secrets.every((s) => !JSON.stringify(e).includes(s) && !JSON.stringify(e).includes(createHash('sha256').update(s).digest('hex')))),
    'no TOTP secret or recovery code (or its hash) in the audit log');

  await del(`/api/v1/admin/users/${user.id}`);
  await del(`/api/v1/admin/profiles/${required.id}`);
}

/** Areas are PostgreSQL schemas, types are tables, fields are typed columns: names, DDL, guards, concurrency, purge. */
async function realTables(x: Json) {
  const { inService, infra, adminMe } = x;
  const ddl = async (q: string) => (await get(`/api/v1/schema-changes?q=${encodeURIComponent(q)}&limit=200`)).json.data.flatMap((c: Json) => c.statements as string[]);
  const detail = async (id: string) => (await get(`/api/v1/configuration-items/${id}`)).json;
  const code = (r: { json: Json }) => `${r.json?.error?.code}/${r.json?.error?.details?.[0]?.code}`;

  console.log('\n# Technical names');
  const suggest = async (q: string) => (await get(`/api/v1/technical-names?${q}`)).json;
  const vmName = await suggest(`kind=type&name=${encodeURIComponent('Virtuelle Maschinen')}&areaId=${infra}`);
  check(vmName.technicalName === 'virtuelle_maschinen' && vmName.derived && vmName.valid && vmName.qualifiedName === 'infrastruktur.virtuelle_maschinen', '"Virtuelle Maschinen" -> virtuelle_maschinen');
  check((await suggest(`kind=field&name=${encodeURIComponent('Größe')}`)).technicalName === 'groesse', '"Größe" -> groesse');
  check((await suggest('kind=area&name=Bestand')).technicalName === 'bestand', '"Bestand" -> bestand');
  const reserved = await suggest('kind=area&name=x&key=pg_temp');
  check(!reserved.valid && reserved.code === 'reserved_prefix', 'a typed pg_ name is reported as reserved');
  const takenArea = await suggest('kind=area&name=Infrastruktur');
  check(!takenArea.valid && takenArea.code === 'name_taken', 'a taken area name is reported before anything is created');
  await get('/api/v1/technical-names?kind=table&name=x', 400);

  console.log('\n# Areas');
  const area = (await post('/api/v1/areas', { name: 'Bestand', description: `Smoke ${RUN}`, color: '#1f6feb', sortOrder: 50 })).json;
  check(area.key === 'bestand' && area.typeCount === 0, 'area "Bestand" gets the technical name bestand');
  check((await ddl('Create area bestand')).includes('CREATE SCHEMA "bestand"'), 'creating the area ran CREATE SCHEMA "bestand"');
  await get(`/api/v1/areas/${area.id}`);
  await get('/api/v1/areas?isActive=true&sort=name&q=bestand');
  await patch(`/api/v1/areas/${area.id}`, { name: 'Bestand (renamed)', icon: 'boxes' });
  check((await get(`/api/v1/areas/${area.id}`)).json.key === 'bestand', 'renaming an area changes only its display name');
  await patch(`/api/v1/areas/${area.id}`, { key: 'other' }, 400); // technical names are immutable
  check(code(await post('/api/v1/areas', { name: 'Bestand' }, 422)) === 'INVALID_NAME/name_taken', 'a taken area name is 422 INVALID_NAME');
  const hostileKeys: [string, string][] = [
    ['bestand"; DROP SCHEMA cmdb CASCADE; --', 'invalid_format'],
    ["x'); DELETE FROM cmdb.users; --", 'invalid_format'],
    ['a;b', 'invalid_format'],
    ['Bestand', 'invalid_format'],
    ['pg_catalog', 'reserved_prefix'],
    ['cmdb', 'reserved_name'],
    ['public', 'reserved_name'],
    ['information_schema', 'reserved_name'],
    ['cmdb_reporting', 'reserved_prefix'],
    ['select', 'reserved_word'],
    ['a'.repeat(64), 'too_long'],
  ];
  for (const [key, why] of hostileKeys) {
    const r = await post('/api/v1/areas', { key, name: 'Injected' }, 422);
    check(code(r) === `INVALID_NAME/${why}` && r.json.error.details[0].field === 'key', `area key ${JSON.stringify(key).slice(0, 44)} is refused (${why})`);
  }
  // A hostile display name never reaches SQL: it only yields a derived identifier.
  const hostile = (await post('/api/v1/areas', { name: `Robert'); DROP TABLE cmdb.users; -- ${RUN}` })).json;
  check(/^robert_drop_table_cmdb_users_[a-z0-9]+$/.test(hostile.key), `a hostile display name yields the plain identifier ${hostile.key}`);
  check((await get('/api/v1/admin/users?limit=1')).json.page.total > 0, 'cmdb.users is untouched');

  console.log('\n# Types are tables');
  const net = (await post('/api/v1/ci-classes', { name: 'Netzwerk', areaId: area.id, icon: 'network' })).json;
  check(net.key === 'netzwerk' && net.tableName === 'bestand.netzwerk' && net.viewName === 'bestand.v_netzwerk', 'type "Netzwerk" in area "Bestand" is the table bestand.netzwerk');
  const history = (await get('/api/v1/schema-changes?q=bestand.netzwerk&sort=-occurredAt')).json;
  const created = history.data.find((c: Json) => c.summary === 'Create type bestand.netzwerk');
  check(created?.statements.includes('CREATE TABLE "bestand"."netzwerk" (id uuid PRIMARY KEY REFERENCES cmdb.configuration_items (id) ON DELETE CASCADE)') &&
    created.statements.some((st: string) => st.startsWith('CREATE VIEW "bestand"."v_netzwerk" AS SELECT')) && created.actorId === adminMe.user.id,
    'the CREATE TABLE and its reporting view ran and are recorded with the acting user');
  await get(`/api/v1/schema-changes/${created.id}`);
  await get('/api/v1/schema-changes/00000000-0000-4000-8000-000000000000', 404);
  const createAudit = (await get(`/api/v1/audit-log?entityType=schema_changes&entityId=${created.id}`)).json;
  check(createAudit.data[0]?.action === 'create' && createAudit.data[0].newValue.statements.length === created.statements.length, 'every schema change is in the audit log');
  const vm = (await post('/api/v1/ci-classes', { name: 'Virtuelle Maschinen', areaId: area.id })).json;
  check(vm.tableName === 'bestand.virtuelle_maschinen', 'type "Virtuelle Maschinen" is the table bestand.virtuelle_maschinen');
  const vmChild = (await post('/api/v1/ci-classes', { key: `smoke_vm_child_${RUN}`, name: 'Smoke VM child', parentId: vm.id })).json;
  const rootNoArea = (await post('/api/v1/ci-classes', { key: `smoke_root_${RUN}`, name: 'Smoke root without area' })).json;
  check(vmChild.areaId === area.id && rootNoArea.areaId === infra && rootNoArea.tableName === `infrastruktur.smoke_root_${RUN}`,
    'without areaId a type goes into its parent\'s area, a root type into "infrastruktur"');
  await del(`/api/v1/ci-classes/${vmChild.id}`);
  await post(`/api/v1/ci-classes/${vmChild.id}/purge`, { confirm: vmChild.key }, 200);
  check(code(await post('/api/v1/ci-classes', { name: 'Netzwerk', areaId: area.id }, 422)) === 'INVALID_NAME/name_taken', 'a taken type name is 422 INVALID_NAME');
  check(code(await post('/api/v1/ci-classes', { key: 'v_netzwerk', name: 'View', areaId: area.id }, 422)) === 'INVALID_NAME/reserved_prefix', 'v_ is reserved for reporting views');
  check(code(await post('/api/v1/ci-classes', { key: 'x"; DROP TABLE cmdb.ci_classes; --', name: 'X', areaId: area.id }, 422)) === 'INVALID_NAME/invalid_format', 'a type key with quotes and semicolons is refused');
  await patch(`/api/v1/ci-classes/${net.id}`, { name: 'Netzwerkgeräte' });
  check((await get(`/api/v1/ci-classes/${net.id}`)).json.tableName === 'bestand.netzwerk', 'renaming a type keeps its table');
  await patch(`/api/v1/ci-classes/${net.id}`, { areaId: infra }, 400); // a type cannot move to another area

  console.log('\n# Fields are typed columns');
  const field = (body: Json, expect = 201) => post('/api/v1/attribute-definitions', { classId: net.id, ...body }, expect);
  const groesse = (await field({ label: 'Größe', dataType: 'integer' })).json;
  check(groesse.key === 'groesse' && (await ddl('groesse')).includes('ALTER TABLE "bestand"."netzwerk" ADD COLUMN "groesse" bigint'), 'field "Größe" adds the column groesse bigint');
  const types: Record<string, string> = { text: 'text', number: 'numeric', boolean: 'boolean', date: 'date', datetime: 'timestamp with time zone', ip: 'inet', cidr: 'cidr' };
  const f: Record<string, Json> = {};
  for (const t of Object.keys(types)) f[t] = (await field({ key: `f_${t}`, label: `F ${t}`, dataType: t })).json;
  const netDdl = await ddl('"bestand"."netzwerk"');
  check(Object.entries(types).every(([t, pg]) => netDdl.includes(`ALTER TABLE "bestand"."netzwerk" ADD COLUMN "f_${t}" ${pg}`)), 'text, number, boolean, date, datetime, ip and cidr map to their PostgreSQL types');
  const rolle = (await field({ key: 'rolle', label: 'Rolle', dataType: 'enum', enumValues: ['core', 'access'] })).json;
  const uplink = (await field({ key: 'uplink', label: 'Uplink', dataType: 'reference', referenceClassId: net.id })).json;
  const hex = (id: string) => id.replaceAll('-', '');
  const netDdl2 = await ddl('"bestand"."netzwerk"');
  check(netDdl2.some((st: string) => st.startsWith(`ALTER TABLE "bestand"."netzwerk" ADD CONSTRAINT "ck_${hex(rolle.id)}_`) && st.endsWith(`CHECK ("rolle" = ANY ('{core,access}'::text[]))`)), 'an enum field is text with a CHECK on its values');
  check(netDdl2.includes(`ALTER TABLE "bestand"."netzwerk" ADD CONSTRAINT "fk_${hex(uplink.id)}" FOREIGN KEY ("uplink") REFERENCES cmdb.configuration_items (id) ON DELETE NO ACTION`), 'a reference field is a foreign key to the registry');
  for (const [key, why] of [['id', 'reserved_name'], ['name', 'reserved_name'], ['a"b', 'invalid_format'], ['x; DROP TABLE y', 'invalid_format'], ['pg_x', 'reserved_prefix'], ['b'.repeat(64), 'too_long'], ['groesse', 'name_taken']]) {
    check(code(await field({ key, label: 'Hostile', dataType: 'text' }, 422)) === `INVALID_NAME/${why}`, `field key ${JSON.stringify(key).slice(0, 30)} is refused (${why})`);
  }
  check(code(await field({ label: 'Name', dataType: 'text' }, 422)) === 'INVALID_NAME/reserved_name', 'a label deriving a registry column name is refused');

  console.log('\n# CIs in type tables');
  const n1 = (await post('/api/v1/configuration-items', {
    classId: net.id, name: `sw-core-${RUN}`, statusId: inService,
    attributes: { groesse: 48, f_text: '42', f_number: 1.5, f_boolean: true, f_date: '2025-01-02', f_datetime: '2025-01-02T03:04:05Z', f_ip: '10.9.0.1', f_cidr: '10.9.0.0/24', rolle: 'core' },
  })).json;
  check(n1.attributes.groesse === 48 && n1.attributes.f_number === 1.5 && n1.attributes.f_boolean === true && n1.attributes.f_date === '2025-01-02' &&
    Date.parse(n1.attributes.f_datetime) === Date.parse('2025-01-02T03:04:05Z') && n1.attributes.f_ip === '10.9.0.1' && n1.attributes.f_cidr === '10.9.0.0/24',
    'a CI of the new type stores and reads back typed values');
  let n2 = (await post('/api/v1/configuration-items', { classId: net.id, name: `sw-access-${RUN}`, statusId: inService, attributes: { f_text: 'not a number', rolle: 'access', uplink: n1.id } })).json;
  check(n2.attributeReferences?.uplink?.name === n1.name, 'a reference field resolves across the type table');
  await post('/api/v1/configuration-items', { classId: net.id, name: 'bad', statusId: inService, attributes: { rolle: 'edge' } }, 400);
  const found = (await get(`/api/v1/search?q=${encodeURIComponent('not a number')}`)).json;
  check(found.data.some((r: Json) => r.item.id === n2.id && r.matches.some((m: Json) => m.field === 'attributes.f_text')), 'search finds values in type tables');
  check((await get(`/api/v1/configuration-items?classId=${net.id}&q=10.9.0.1`)).json.data.some((c: Json) => c.id === n1.id), 'the inventory filter searches type columns');

  console.log('\n# Data-loss guards');
  const toNumber = { operation: 'updateField', id: f.text.id, body: { dataType: 'number' } };
  const refusedPreview = await post('/api/v1/schema-changes/preview', toNumber, 422);
  check(code(refusedPreview) === 'SCHEMA_CHANGE_REFUSED/type_change_failed' && /"not a number"/.test(refusedPreview.json.error.message), 'preview: a type change that would not convert every value is refused, naming the value');
  check(code(await patch(`/api/v1/attribute-definitions/${f.text.id}`, { dataType: 'number' }, 422)) === 'SCHEMA_CHANGE_REFUSED/type_change_failed', 'the type change itself is refused');
  check((await get(`/api/v1/attribute-definitions/${f.text.id}`)).json.dataType === 'text', 'a refused change leaves the field as it was');
  n2 = (await patch(`/api/v1/configuration-items/${n2.id}`, { version: n2.version, attributes: { f_text: '7' } })).json;
  const preview = (await post('/api/v1/schema-changes/preview', toNumber, 200)).json;
  check(preview.statements.some((st: string) => st.startsWith('ALTER TABLE "bestand"."netzwerk" ALTER COLUMN "f_text" TYPE numeric USING')) &&
    preview.impact.some((i: Json) => i.kind === 'rewrite' && i.rows === 2) && preview.result?.dataType === 'number', 'preview: the ALTER COLUMN TYPE and the 2 values it converts');
  check((await get(`/api/v1/attribute-definitions/${f.text.id}`)).json.dataType === 'text', 'a preview changes nothing');
  await patch(`/api/v1/attribute-definitions/${f.text.id}`, { dataType: 'number' });
  check((await detail(n1.id)).attributes.f_text === 42 && (await detail(n2.id)).attributes.f_text === 7, 'converted values read back as numbers');
  check(code(await patch(`/api/v1/attribute-definitions/${f.ip.id}`, { isRequired: true }, 422)) === 'SCHEMA_CHANGE_REFUSED/values_missing', 'a field cannot become required while an asset has no value');
  n2 = (await patch(`/api/v1/configuration-items/${n2.id}`, { version: n2.version, attributes: { f_ip: '10.9.0.2' } })).json;
  await patch(`/api/v1/attribute-definitions/${f.ip.id}`, { isRequired: true });
  check((await ddl('f_ip')).includes('ALTER TABLE "bestand"."netzwerk" ALTER COLUMN "f_ip" SET NOT NULL'), 'once every asset has a value the column becomes NOT NULL');
  await post('/api/v1/configuration-items', { classId: net.id, name: 'no-ip', statusId: inService }, 400);

  console.log('\n# Concurrent schema changes');
  const burst = await Promise.all([0, 1, 2, 3].map((i) => call('POST', '/api/v1/attribute-definitions', { classId: vm.id, key: `port_${i}`, label: `Port ${i}`, dataType: 'integer' }, 201)));
  const vmDdl = await ddl('"bestand"."virtuelle_maschinen"');
  check(burst.every((r) => r.status === 201) && [0, 1, 2, 3].every((i) => vmDdl.includes(`ALTER TABLE "bestand"."virtuelle_maschinen" ADD COLUMN "port_${i}" bigint`)), 'four fields added at once all get their column');
  const twice = await Promise.all([0, 1].map(() => call('POST', '/api/v1/attribute-definitions', { classId: vm.id, key: 'mac', label: 'MAC', dataType: 'text' }, undefined, {}, { accept: [201, 422] })));
  check(twice.map((r) => r.status).sort().join() === '201,422' && twice.some((r) => code(r) === 'INVALID_NAME/name_taken'), 'the same field added twice at once: one 201, one 422 (the schema lock serialises them)');
  const reconciled = (await post('/api/v1/schema-changes/reconcile', undefined, 200)).json;
  check(reconciled.schemaChange === null, 'after all that the database matches the data model (reconcile has nothing to do)');

  console.log('\n# Archive and purge');
  await del(`/api/v1/attribute-definitions/${groesse.id}`);
  check((await detail(n1.id)).attributes.groesse === 48, 'an archived field keeps its column and values');
  await post(`/api/v1/attribute-definitions/${f.date.id}/purge`, { confirm: 'f_date' }, 409); // not archived
  await post(`/api/v1/attribute-definitions/${groesse.id}/purge`, { confirm: 'wrong' }, 400);
  const purgedField = (await post(`/api/v1/attribute-definitions/${groesse.id}/purge`, { confirm: 'groesse' }, 200)).json;
  check(purgedField.schemaChange.statements.includes('ALTER TABLE "bestand"."netzwerk" DROP COLUMN "groesse"') &&
    purgedField.schemaChange.impact.some((i: Json) => i.kind === 'drop_column' && i.rows === 1), 'purging a field drops its column (1 stored value)');
  check(!('groesse' in (await detail(n1.id)).attributes), 'the purged value is gone');
  await get(`/api/v1/attribute-definitions/${groesse.id}`, 404);
  await del(`/api/v1/ci-classes/${vm.id}`);
  const purgePreview = (await post('/api/v1/schema-changes/preview', { operation: 'purgeType', id: vm.id, body: { confirm: 'virtuelle_maschinen' } }, 200)).json;
  check(purgePreview.statements.includes('DROP TABLE "bestand"."virtuelle_maschinen"') && purgePreview.result === null, 'preview of a type purge shows the DROP TABLE');
  await get(`/api/v1/ci-classes/${vm.id}`); // still there
  await post(`/api/v1/ci-classes/${vm.id}/purge`, { confirm: 'virtuelle_maschinen' }, 200);
  await post(`/api/v1/areas/${area.id}/purge`, { confirm: 'bestand' }, 409); // not archived
  await del(`/api/v1/areas/${area.id}`);
  const stillTyped = await post(`/api/v1/areas/${area.id}/purge`, { confirm: 'bestand' }, 409);
  check(stillTyped.json.error?.code === 'IN_USE', 'an area holding types cannot be purged');
  await del(`/api/v1/ci-classes/${net.id}`);
  const purgedType = (await post(`/api/v1/ci-classes/${net.id}/purge`, { confirm: 'netzwerk' }, 200)).json;
  check(purgedType.schemaChange.statements.includes('DROP TABLE "bestand"."netzwerk"') && purgedType.schemaChange.statements.includes('DROP VIEW "bestand"."v_netzwerk"'), 'purging a type drops its table and view');
  await get(`/api/v1/configuration-items/${n1.id}`, 404);
  const purgedArea = (await post(`/api/v1/areas/${area.id}/purge`, { confirm: 'bestand' }, 200)).json;
  check(purgedArea.schemaChange.statements.includes('DROP SCHEMA "bestand"'), 'purging the empty area drops its schema');
  await del(`/api/v1/areas/${hostile.id}`);
  await post(`/api/v1/areas/${hostile.id}/purge`, { confirm: hostile.key }, 200);
}

const PNG_1X1 = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==';
const b64 = (s: string | Uint8Array) => Buffer.from(s).toString('base64');
const fields = (r: { json: Json }) => (r.json?.error?.details ?? []).map((d: Json) => d.field as string);

/** UI settings (versioned, audited), logo and favicon, configuration export and import. */
async function customization(x: Json) {
  const { serverClass, adminMe } = x;
  const put = (url: string, body: unknown, expect = 200) => call('PUT', url, body, expect);

  console.log('\n# UI settings');
  const before = (await get('/api/v1/ui-settings')).json;
  const firstVersion: number = before.version;
  const serverKey: string = (await get(`/api/v1/ci-classes/${serverClass}`)).json.key;
  const gone = `gone_${RUN}`;
  const doc = {
    branding: { appName: `Smoke CMDB ${RUN}`, primaryColor: '#1f6feb', accentColor: null, defaultTheme: 'dark' },
    navigation: {
      entries: [
        { type: 'page', page: 'dashboard' },
        { type: 'section', key: 'infra', label: 'Infrastructure', items: [{ classKey: serverKey, label: 'Hosts' }, { classKey: gone }] },
        { type: 'page', page: 'audit_log', hidden: true },
      ],
    },
    dashboard: {
      widgets: [
        { id: 'by_class', type: 'count_by_class', classKeys: [serverKey] },
        { id: 'recent', type: 'recent_changes', limit: 5, size: 'large' },
        { id: 'prod', type: 'saved_search', title: 'Production servers', search: { classKeys: [serverKey], filters: { environmentKeys: ['production'], statusKeys: [gone] } } },
      ],
    },
    listViews: [{ classKey: serverKey, columns: ['name', 'status', 'attributes.cpu_cores', `attributes.${gone}`], defaultSort: { field: 'name', direction: 'desc' }, pageSize: 25 }],
    layouts: [{ classKey: serverKey, panels: [{ key: 'main', label: 'Main', fields: ['name', 'hostname'] }], hiddenFields: ['notes'], readOnlyFields: ['serialNumber'] }],
  };
  const stale = await put('/api/v1/ui-settings', { version: firstVersion + 1000, settings: doc }, 409);
  check(stale.json.error?.code === 'VERSION_CONFLICT', 'saving over a newer version is a VERSION_CONFLICT');
  const invalid = await put('/api/v1/ui-settings', { version: firstVersion, settings: { navigation: { entries: [{ type: 'page' }] }, layouts: [{ classKey: serverKey, hiddenFields: ['name'] }] } }, 400);
  check(fields(invalid).includes('settings.navigation.entries.0.page') && fields(invalid).includes('settings.layouts.0.hiddenFields.0'), 'cross-field rules report paths into the document');
  await put('/api/v1/ui-settings', { version: firstVersion, settings: { branding: { primaryColor: 'blue' } } }, 400);
  await put('/api/v1/ui-settings', { version: firstVersion, settings: { menu: [] } }, 400);
  const saved = (await put('/api/v1/ui-settings', { version: firstVersion, settings: doc, comment: 'smoke' })).json;
  const codes = saved.issues.map((i: Json) => i.code);
  check(saved.version === firstVersion + 1 && codes.includes('unknown_class') && codes.includes('unknown_attribute') && codes.includes('unknown_status'), 'dangling references are accepted and reported as issues');
  check(saved.settings.navigation.entries[1].items.length === 1 && !saved.settings.listViews[0].columns.includes(`attributes.${gone}`), 'the effective settings leave dangling references out');
  const same = (await put('/api/v1/ui-settings', { version: saved.version, settings: doc })).json;
  check(same.version === saved.version, 'saving an unchanged document creates no version');
  const branding = (await as(null, () => get('/api/v1/ui-settings/branding'))).json;
  check(branding.appName === `Smoke CMDB ${RUN}` && branding.defaultTheme === 'dark' && branding.primaryColor === '#1f6feb', 'branding is public for the login page');
  const versions = (await get('/api/v1/ui-settings/versions?limit=2')).json;
  check(versions.data[0]?.version === saved.version && versions.data[0].isCurrent && versions.data[0].comment === 'smoke' && versions.data[0].actorName === ADMIN_USERNAME, 'versions list the saved document, newest first');
  const v1 = (await get(`/api/v1/ui-settings/versions/${firstVersion}`)).json;
  check(v1.isCurrent === false && typeof v1.settings.branding === 'object', 'an earlier version keeps its document');
  await get('/api/v1/ui-settings/versions/999999', 404);
  await get('/api/v1/ui-settings/versions/abc', 400);
  const trail = (await get('/api/v1/audit-log?entityType=ui_settings&limit=1')).json;
  check(trail.data[0]?.newValue?.version === saved.version && trail.data[0].oldValue?.version === firstVersion && trail.data[0].actorId === adminMe.user.id, 'settings changes are audited with both versions');

  console.log('\n# Logo and favicon');
  const originals: Record<string, Json> = {};
  for (const kind of ['logo', 'favicon']) {
    const a = saved.assets[kind];
    if (a) originals[kind] = { contentType: a.contentType, data: b64((await get(`/api/v1/ui-settings/assets/${kind}`)).bytes) };
  }
  const logo = (await put('/api/v1/ui-settings/assets/logo', { contentType: 'image/png', data: PNG_1X1 })).json;
  check(logo.kind === 'logo' && logo.size === 70 && logo.url.startsWith('/api/v1/ui-settings/assets/logo?v='), 'logo uploaded');
  const img = await as(null, () => get('/api/v1/ui-settings/assets/logo'));
  check(img.headers.get('content-type') === 'image/png' && b64(img.bytes) === PNG_1X1, 'the logo is served as uploaded, without a session');
  check(/sandbox/.test(img.headers.get('content-security-policy') ?? '') && img.headers.get('x-content-type-options') === 'nosniff', 'images are served with a sandboxing CSP and nosniff');
  await as(null, () => call('GET', '/api/v1/ui-settings/assets/logo', undefined, 304, { 'if-none-match': img.headers.get('etag') ?? '' }));
  await put('/api/v1/ui-settings/assets/favicon', { contentType: 'image/jpeg', data: b64('\xff\xd8\xff') }, 400);
  const script = await put('/api/v1/ui-settings/assets/logo', { contentType: 'image/svg+xml', data: b64('<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"/>') }, 400);
  check(script.json.error?.details?.[0]?.code === 'unsafe_content', 'SVGs with scripts are refused');
  await put('/api/v1/ui-settings/assets/logo', { contentType: 'image/jpeg', data: PNG_1X1 }, 400);
  await put('/api/v1/ui-settings/assets/logo', { contentType: 'image/png', data: 'not base64!' }, 400);
  const big = new Uint8Array(130 * 1024);
  big.set(Buffer.from(PNG_1X1, 'base64'));
  const tooBig = await put('/api/v1/ui-settings/assets/favicon', { contentType: 'image/png', data: b64(big) }, 400);
  check(tooBig.json.error?.details?.[0]?.code === 'too_big', 'favicons are limited to 128 KiB');
  await put('/api/v1/ui-settings/assets/banner', { contentType: 'image/png', data: PNG_1X1 }, 400);
  await put('/api/v1/ui-settings/assets/favicon', { contentType: 'image/svg+xml', data: b64('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><rect width="1" height="1"/></svg>') });
  const withImages = (await as(null, () => get('/api/v1/ui-settings/branding'))).json;
  check(withImages.logo?.contentType === 'image/png' && withImages.favicon?.contentType === 'image/svg+xml', 'branding lists both images');

  console.log('\n# Configuration export/import');
  const scratch = (await post('/api/v1/statuses', { key: `smoke_exp_${RUN}`, name: 'Exported status' })).json;
  const exported = await get('/api/v1/admin/config/export');
  const file = exported.json;
  const raw = JSON.stringify(file);
  check(/^attachment; filename="shadoucmdb-config-/.test(exported.headers.get('content-disposition') ?? ''), 'the export downloads as a file');
  check(file.format === 'shadoucmdb.config' && file.formatVersion === 2 && file.dataModel.areas.some((a: Json) => a.key === 'infrastruktur') && !('users' in file) && !raw.includes('argon2') && !raw.includes('"username"') && !raw.includes('password'), 'the export has no users or password hashes');
  check(file.permissionProfiles.every((p: Json) => p.name !== 'Administrator') && file.uiSettings.logo?.data === PNG_1X1, 'the export has editable profiles and the images');
  check(file.dataModel.attributes.every((a: Json) => typeof a.class === 'string' && !('classId' in a)), 'the export refers to classes by key');
  const noop = (await post('/api/v1/admin/config/import?mode=dry_run', file, 200)).json;
  check(noop.applied === false && noop.changes.length === 0 && noop.summary.every((s: Json) => s.created + s.updated + s.deleted === 0), `importing an install's own export changes nothing (${JSON.stringify(noop.changes).slice(0, 300)})`);

  const changed = structuredClone(file);
  changed.lookups.statuses.find((s: Json) => s.key === scratch.key).name = 'Renamed by import';
  changed.lookups.statuses.push({ key: `smoke_imp_${RUN}`, name: 'Imported status' });
  changed.dataModel.classes.push({ key: `smoke_imp_${RUN}`, name: 'Imported class', parent: serverKey });
  changed.dataModel.attributes.push({ class: `smoke_imp_${RUN}`, key: 'rack_unit', label: 'Rack unit', dataType: 'integer', validation: { min: 1 } });
  changed.uiSettings.settings.branding.appName = `Imported ${RUN}`;
  changed.uiSettings.favicon = null;
  const dry = (await post('/api/v1/admin/config/import?mode=dry_run', changed, 200)).json;
  const change = (section: string, key: string) => dry.changes.find((c: Json) => c.section === section && c.key === key);
  check(change('statuses', scratch.key)?.action === 'update' && change('statuses', scratch.key).fields[0]?.to === 'Renamed by import', 'dry run: updated fields with old and new values');
  check(change('statuses', `smoke_imp_${RUN}`)?.action === 'create' && change('classes', `smoke_imp_${RUN}`)?.action === 'create' && change('attributes', `smoke_imp_${RUN}.rack_unit`)?.action === 'create', 'dry run: created rows');
  check(change('uiSettings', 'settings')?.action === 'update' && change('uiSettings', 'favicon')?.action === 'delete', 'dry run: UI settings and images');
  check(dry.schemaChanges.some((c: Json) => c.statements.some((st: string) => st.startsWith(`CREATE TABLE "infrastruktur"."smoke_imp_${RUN}"`))) &&
    dry.schemaChanges.some((c: Json) => c.statements.some((st: string) => st === `ALTER TABLE "infrastruktur"."smoke_imp_${RUN}" ADD COLUMN "rack_unit" bigint`)),
    'the import dry run shows the DDL it would run');
  check(dry.summary.find((s: Json) => s.section === 'statuses')?.notInFile === 0, 'dry run: counts rows missing from the file');
  check((await get(`/api/v1/statuses?q=smoke_imp_${RUN}`)).json.page.total === 0 && (await get(`/api/v1/statuses/${scratch.id}`)).json.name === 'Exported status', 'a dry run changes nothing');
  const applied = (await post('/api/v1/admin/config/import?mode=apply', changed, 200)).json;
  check(applied.applied === true && applied.changes.length === dry.changes.length, 'apply makes the changes the dry run reported');
  const imported = (await get(`/api/v1/statuses?q=smoke_imp_${RUN}`)).json.data[0];
  check(imported && (await get(`/api/v1/statuses/${scratch.id}`)).json.name === 'Renamed by import', 'imported rows exist');
  const after = (await as(null, () => get('/api/v1/ui-settings/branding'))).json;
  check(after.appName === `Imported ${RUN}` && after.favicon === null && after.logo !== null, 'imported UI settings and images apply');
  const importAudit = (await get(`/api/v1/audit-log?entityType=statuses&entityId=${imported.id}`)).json;
  check(importAudit.data[0]?.action === 'create' && importAudit.data[0].actorId === adminMe.user.id, 'imported rows are audited with the importing user');
  const again = (await post('/api/v1/admin/config/import?mode=apply', changed, 200)).json;
  check(again.changes.length === 0, 're-importing the same file changes nothing');

  const broken = await post('/api/v1/admin/config/import?mode=dry_run', {
    format: 'shadoucmdb.config',
    formatVersion: 1,
    dataModel: { attributes: [{ class: `nope_${RUN}`, key: 'x', label: 'X', dataType: 'enum' }] },
    lookups: { statuses: [{ key: `dup_${RUN}`, name: 'A' }, { key: `dup_${RUN}`, name: 'B' }] },
  }, 400);
  check(['dataModel.attributes.0.class', 'dataModel.attributes.0.enumValues', 'lookups.statuses.1'].every((f) => fields(broken).includes(f)), 'every problem in the file is reported with its path');
  const retyped = structuredClone(file);
  const attr = retyped.dataModel.attributes.find((a: Json) => a.dataType === 'integer' || a.dataType === 'number');
  attr.dataType = 'text';
  const immutable = await post('/api/v1/admin/config/import?mode=dry_run', retyped, 400);
  check(immutable.json.error?.details?.some((d: Json) => d.code === 'immutable'), 'the data type of an existing attribute cannot change');
  await post('/api/v1/admin/config/import?mode=apply', { format: 'shadoucmdb.config', formatVersion: 3 }, 400);
  await post('/api/v1/admin/config/import', file, 400); // mode is required
  await post('/api/v1/admin/config/import?mode=later', file, 400);

  console.log('\n# Export/import permission');
  const password = `importer-${RUN}-password`;
  const importers = (await post('/api/v1/admin/profiles', { name: `smoke-importers-${RUN}`, globalPermissions: ['config.export_import'] })).json;
  const importer = (await post('/api/v1/admin/users', { username: `smoke-importer-${RUN}`, displayName: 'Importer', password, profileIds: [importers.id] })).json;
  await as(await login(importer.username, password), async () => {
    await get('/api/v1/admin/config/export');
    const esc = await post('/api/v1/admin/config/import?mode=apply', { format: 'shadoucmdb.config', formatVersion: 1, permissionProfiles: [{ name: `smoke-escalate-${RUN}`, globalPermissions: ['users.manage'] }] }, 403);
    check(esc.json.error?.code === 'FORBIDDEN', 'an import cannot create a profile granting more than the importer holds');
    await put('/api/v1/ui-settings', { version: 1, settings: {} }, 403);
    await get('/api/v1/ui-settings'); // any signed-in user reads the settings
  });

  // Clean up and put the settings and images back.
  await del(`/api/v1/admin/users/${importer.id}`);
  await del(`/api/v1/admin/profiles/${importers.id}`);
  const attrs = (await get(`/api/v1/attribute-definitions?q=rack_unit&limit=200`)).json.data;
  const newClass = (await get(`/api/v1/ci-classes?q=smoke_imp_${RUN}`)).json.data[0];
  for (const a of attrs.filter((a: Json) => a.classId === newClass.id)) await del(`/api/v1/attribute-definitions/${a.id}`);
  await del(`/api/v1/ci-classes/${newClass.id}`);
  await del(`/api/v1/statuses/${imported.id}`);
  await del(`/api/v1/statuses/${scratch.id}`);
  const current = (await get('/api/v1/ui-settings')).json;
  const restored = (await post(`/api/v1/ui-settings/versions/${firstVersion}/restore`, { version: current.version }, 200)).json;
  check(restored.version === current.version + 1 && JSON.stringify(restored.settings) === JSON.stringify(before.settings), 'restoring an earlier version saves it as a new version');
  await post(`/api/v1/ui-settings/versions/${firstVersion}/restore`, { version: current.version }, 409);
  await del('/api/v1/ui-settings/assets/logo');
  await del('/api/v1/ui-settings/assets/logo', 404);
  await get('/api/v1/ui-settings/assets/logo', 404);
  for (const [kind, asset] of Object.entries(originals)) await put(`/api/v1/ui-settings/assets/${kind}`, asset);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
