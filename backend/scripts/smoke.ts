/*
 * End-to-end smoke test against a running API (which must be connected to a
 * real, migrated and seeded PostgreSQL). It exercises every operation in the
 * OpenAPI document, including the error paths, and fails if any operation
 * was not called, any status is unexpected, or anything returns 5xx.
 *
 *   API_URL=http://localhost:3000 npm run smoke -w backend
 *
 * Run the API with NODE_ENV=development (or test) so it also checks each
 * response against its OpenAPI schema; a mismatch surfaces here as a 500.
 * The script only creates rows with a unique run suffix; it never deletes seed data.
 */

const BASE = (process.env.API_URL ?? '').replace(/\/$/, '');
if (!BASE) {
  console.error('Set API_URL, e.g. API_URL=http://localhost:3000');
  process.exit(2);
}
const RUN = Date.now().toString(36);
const VERBOSE = process.argv.includes('--verbose');

type Json = any; // eslint-disable-line @typescript-eslint/no-explicit-any

interface Op {
  method: string;
  path: string;
  operationId: string;
  regex: RegExp;
}
let ops: Op[] = [];
const covered = new Set<string>();
const failures: string[] = [];
let calls = 0;

function operationFor(method: string, url: string): string | undefined {
  const path = url.split('?')[0]!;
  return ops.find((o) => o.method === method && o.regex.test(path))?.operationId;
}

async function call(method: string, url: string, body?: unknown, expect?: number, headers: Record<string, string> = {}): Promise<{ status: number; json: Json }> {
  const res = await fetch(BASE + url, {
    method,
    headers: {
      'x-actor-name': 'smoke-test',
      ...(body !== undefined ? { 'content-type': 'application/json' } : {}),
      ...headers,
    },
    body: body === undefined ? undefined : typeof body === 'string' ? body : JSON.stringify(body),
  });
  const text = await res.text();
  const json = text ? JSON.parse(text) : undefined;
  calls++;
  const op = operationFor(method, url);
  if (op) covered.add(op);
  const ok = (expect === undefined ? res.status < 400 : res.status === expect) && res.status < 500;
  const summary = summarise(json);
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${method.padEnd(6)} ${url} -> ${res.status}${summary ? `  ${summary}` : ''}`);
  if (VERBOSE && json) console.log(JSON.stringify(json, null, 1).slice(0, 2000));
  if (!ok) failures.push(`${method} ${url}: expected ${expect ?? '2xx'}, got ${res.status} ${text.slice(0, 400)}`);
  return { status: res.status, json };
}

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
  if (!row) throw new Error(`seed row ${collection}/${key} not found; run npm run db:seed first`);
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
      regex: new RegExp(`^${path.replace(/\{[^}]+\}/g, '[^/]+')}$`),
    })),
  );
  console.log(`# OpenAPI ${spec.openapi}: ${ops.length} operations`);
  await get('/healthz');
  const ready = await get('/readyz');
  check(ready.json.migrations?.upToDate === true, 'readyz reports migrations up to date');

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
      history.data.every((e: Json) => e.actorName === 'smoke-test' && e.requestId),
    'audit log has create/update/delete with old/new values, actor and request id',
  );
  await get('/api/v1/audit-log?actorName=smoke&action=delete&limit=5');
  await get('/api/v1/audit-log?from=not-a-date', 400);

  // --- HTTP-level errors ---------------------------------------------------------
  console.log('\n# HTTP errors');
  await call('POST', '/api/v1/statuses', '{"key":', 400);
  await call('POST', '/api/v1/statuses', 'key=x', 415, { 'content-type': 'application/x-www-form-urlencoded' });
  await get('/api/v1/does-not-exist', 404);
  await get('/api/v1/configuration-items?limit=500', 400);

  // --- Coverage -------------------------------------------------------------------
  const missing = ops.filter((o) => !covered.has(o.operationId));
  console.log(`\n# ${calls} requests, ${covered.size}/${ops.length} OpenAPI operations exercised`);
  for (const m of missing) failures.push(`operation not exercised: ${m.method} ${m.path} (${m.operationId})`);
  if (failures.length) {
    console.log(`\n${failures.length} FAILURE(S):\n- ${failures.join('\n- ')}`);
    process.exit(1);
  }
  console.log('ALL CHECKS PASSED');
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
