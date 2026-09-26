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
): Promise<{ status: number; json: Json; headers: Headers }> {
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
  const text = await res.text();
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
  return { status: res.status, json, headers: res.headers };
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
  const PUBLIC = ['getLiveness', 'getReadiness', 'getSetupStatus', 'completeSetup', 'login'];
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

  // --- Classes and attribute definitions ----------------------------------------
  console.log('\n# CI classes / attribute definitions');
  const hardware = await idByKey('ci-classes', 'hardware');
  const networkDevice = await idByKey('ci-classes', 'network_device');
  const serverClass = await idByKey('ci-classes', 'server');
  const appClass = await idByKey('ci-classes', 'application');
  const dbClass = await idByKey('ci-classes', 'database');
  await get(`/api/v1/ci-classes?descendantOf=${hardware}`);
  const lb = (await post('/api/v1/ci-classes', { key: `smoke_lb_${RUN}`, name: 'Smoke load balancer', parentId: networkDevice })).json;
  await get(`/api/v1/ci-classes/${lb.id}`);
  await patch(`/api/v1/ci-classes/${lb.id}`, { description: 'LB for smoke test', icon: 'scale' });
  await patch(`/api/v1/ci-classes/${hardware}`, { parentId: lb.id }, 400); // cycle
  await patch(`/api/v1/ci-classes/${lb.id}`, { key: 'renamed' }, 400); // key immutable
  const algo = (await post('/api/v1/attribute-definitions', {
    classId: lb.id, key: 'algorithm', label: 'Algorithm', dataType: 'enum', enumValues: ['round_robin', 'least_conn'], isRequired: true,
  })).json;
  const vip = (await post('/api/v1/attribute-definitions', { classId: lb.id, key: 'vip', label: 'Virtual IP', dataType: 'ip' })).json;
  await post('/api/v1/attribute-definitions', { classId: lb.id, key: 'model', label: 'Model again', dataType: 'text' }, 409); // defined on hardware
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
  await post('/api/v1/configuration-items', { classId: hardware, name: 'abstract', statusId: inService }, 400);
  await post('/api/v1/configuration-items', {
    classId: serverClass, name: 'bad', statusId: inService, hostname: '-bad-', ipAddress: '10.1.1.300',
  }, 400);
  await post('/api/v1/configuration-items', {
    classId: appClass, name: 'bad-attrs', statusId: inService, attributes: { url: 'ftp://x', criticality: 'extreme', primary_database: server.id, nope: 1 },
  }, 400);
  await post('/api/v1/configuration-items', { classId: lb.id, name: 'lb-missing-required', statusId: inService, attributes: { vip: '10.0.0.1' } }, 400);
  const lbItem = (await post('/api/v1/configuration-items', {
    classId: lb.id, name: `smoke-lb-${RUN}`, statusId: inService, attributes: { device_role: 'load_balancer', algorithm: 'round_robin', vip: '10.77.5.5', management_subnet: '10.77.5.0/24' },
  })).json;
  await post('/api/v1/configuration-items', { classId: lb.id, name: 'bad-cidr', statusId: inService, attributes: { device_role: 'load_balancer', algorithm: 'least_conn', management_subnet: '10.77.5.1/24' } }, 400); // host bits set
  await patch(`/api/v1/attribute-definitions/${algo.id}`, { enumValues: ['least_conn'] }, 400); // round_robin in use

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
  await del(`/api/v1/relationship-rules/${rule.id}`);
  await del(`/api/v1/relationship-types/${rt.id}`);
  await del(`/api/v1/attribute-definitions/${vip.id}`, 409); // lb item stored a vip value
  await patch(`/api/v1/ci-classes/${lb.id}`, { isActive: false });
  await del(`/api/v1/ci-classes/${lb.id}`, 409);
  await del(`/api/v1/locations/${room.id}`, 409);

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
  const { serverClass, appClass, server, app, database, r1, inService, adminMe } = x;
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
  const keys = (e: Json) => Object.keys(e?.newValue ?? {}).sort().join(',');
  check(failure && failure.actorId === null && failure.newValue.attemptedUsername === reader.username && failure.newValue.ipAddress === '203.0.113.38',
    'login.failure: no actor id, the attempted username and the client IP');
  check(ghostFailure && keys(failure) === 'attemptedUsername,ipAddress,userAgent' && keys(ghostFailure) === keys(failure),
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

  // Clean up what only this run uses.
  await del(`/api/v1/admin/users/${reader.id}`);
  await del(`/api/v1/admin/users/${nobody.id}`);
  for (const p of [readers, editors, copy, userManagers]) await del(`/api/v1/admin/profiles/${p.id}`);
  await del(`/api/v1/admin/profiles/${readers.id}`, 404);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
