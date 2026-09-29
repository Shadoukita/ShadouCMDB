/*
 * In-place upgrade check: create representative data through the API of an
 * older release, upgrade the database, then prove through the API of the new
 * release that every object is still there and unchanged.
 *
 *   API_URL=http://127.0.0.1:3000 node tools/upgrade/upgrade-check.ts seed  > snapshot.json   # old release
 *   API_URL=http://127.0.0.1:3000 node tools/upgrade/upgrade-check.ts check < snapshot.json   # after the upgrade
 *
 * Node.js 22.18+, no dependencies. Run against a migrated database without
 * users, ideally with the old release's demo inventory (`seed --demo`):
 * `seed` completes first-run setup as "upgrade-admin" with UPGRADE_PASSWORD,
 * creates "upgrade-viewer" with UPGRADE_VIEWER_PASSWORD, and uses only API
 * operations that exist since v0.1.0-rc.1, so it works against every tag in
 * the CI matrix (.github/workflows/upgrade.yml).
 *
 * `seed` prints the snapshot to stdout (progress goes to stderr): the ids it created, the full GET response of
 * those objects and of every CI, class and relationship in the database, the
 * whole audit log, and what the restricted user could see.
 * `check` reads every object again and fails when a field that was there
 * before is missing or has another value. New fields are fine: the newer
 * release may return more. Fields the newer release changed on purpose are
 * listed in CHANGED_ON_PURPOSE, each with the reason.
 */

const BASE = (process.env.API_URL ?? '').replace(/\/$/, '');
const MODE = process.argv[2];
if (!BASE || !['seed', 'check'].includes(MODE ?? '')) {
  console.error('Usage: API_URL=http://127.0.0.1:3000 node tools/upgrade/upgrade-check.ts seed > snapshot.json | check < snapshot.json');
  process.exit(2);
}
const ADMIN = 'upgrade-admin';
const ADMIN_PASSWORD = process.env.UPGRADE_PASSWORD ?? 'upgrade-admin-password';
const VIEWER = 'upgrade-viewer';
const VIEWER_PASSWORD = process.env.UPGRADE_VIEWER_PASSWORD ?? 'upgrade-viewer-password';

type Json = any; // eslint-disable-line @typescript-eslint/no-explicit-any

interface Identity {
  cookie: string;
  csrf: string;
}
let me: Identity | null = null;
const failures: string[] = [];

async function call(method: string, url: string, body?: unknown): Promise<{ status: number; json: Json; headers: Headers }> {
  const res = await fetch(BASE + url, {
    method,
    headers: {
      ...(me ? { cookie: me.cookie } : {}),
      ...(me && method !== 'GET' ? { 'x-csrf-token': me.csrf } : {}),
      ...(body !== undefined ? { 'content-type': 'application/json' } : {}),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let json: Json;
  try {
    json = text ? JSON.parse(text) : undefined;
  } catch {
    json = text;
  }
  console.error(`${method.padEnd(6)} ${url} -> ${res.status}`);
  return { status: res.status, json, headers: res.headers };
}

/** A call that must succeed; returns the body. */
async function ok(method: string, url: string, body?: unknown): Promise<Json> {
  const r = await call(method, url, body);
  if (r.status >= 400) throw new Error(`${method} ${url}: ${r.status} ${JSON.stringify(r.json).slice(0, 600)}`);
  return r.json;
}

function identity(res: { headers: Headers; json: Json }): Identity {
  const cookies = res.headers.getSetCookie().map((c) => c.split(';')[0]!);
  const cookie = cookies.find((c) => c.startsWith('shadoucmdb_session='));
  if (!cookie) throw new Error('no session cookie in the response');
  return { cookie, csrf: res.json?.csrfToken ?? '' };
}

async function login(username: string, password: string): Promise<Identity> {
  me = null;
  const r = await call('POST', '/api/v1/auth/login', { username, password });
  if (r.status !== 200) throw new Error(`login as ${username}: ${r.status} ${JSON.stringify(r.json).slice(0, 400)}`);
  return identity(r);
}

/** Every page of a list endpoint. */
async function all(url: string): Promise<Json[]> {
  const out: Json[] = [];
  for (let offset = 0; ; ) {
    const sep = url.includes('?') ? '&' : '?';
    const page = await ok('GET', `${url}${sep}limit=100&offset=${offset}`);
    out.push(...page.data);
    offset += page.data.length;
    if (page.data.length === 0 || offset >= page.page.total) return out;
  }
}

// --- What the restricted user can do: part of the snapshot, compared after the upgrade ---
interface ViewerView {
  visibleCiIds: string[];
  /** Status of GET on each seeded CI, by CI id. */
  getStatus: Record<string, number>;
  /** Status of a create in a class the profile grants only `view` on. */
  createStatus: number;
  /** Status of GET /api/v1/audit-log (the profile has no audit.view). */
  auditStatus: number;
}

async function viewerView(ids: Json): Promise<ViewerView> {
  me = await login(VIEWER, VIEWER_PASSWORD);
  const visibleCiIds = (await all('/api/v1/configuration-items')).map((c) => c.id).sort();
  const getStatus: Record<string, number> = {};
  for (const id of Object.values(ids.cis) as string[]) getStatus[id] = (await call('GET', `/api/v1/configuration-items/${id}`)).status;
  // The body must be valid for the release being asked, or its 400 would hide the 403:
  // the fixed fields (name, statusId) became class attributes in migration 0016.
  const body = MODE === 'seed' ? { classId: ids.classes.server, name: 'upg-viewer-denied', statusId: ids.status } : { classId: ids.classes.server };
  const createStatus = (await call('POST', '/api/v1/configuration-items', body)).status;
  const auditStatus = (await call('GET', '/api/v1/audit-log?limit=1')).status;
  me = null;
  return { visibleCiIds, getStatus, createStatus, auditStatus };
}

// --- seed: runs against the old release ---
async function seed() {
  const setup = await call('GET', '/api/v1/setup');
  if (!setup.json?.setupRequired) throw new Error('first-run setup is already done: seed needs an empty database');
  me = identity(await call('POST', '/api/v1/setup', { username: ADMIN, displayName: 'Upgrade Admin', password: ADMIN_PASSWORD }));

  // Newer releases group types in areas. Create one when the release has them.
  const hasAreas = (await call('GET', '/api/v1/areas')).status === 200;
  const area = hasAreas ? await ok('POST', '/api/v1/areas', { key: 'upg_area', name: 'Upgrade area' }) : null;
  const inArea = area ? { areaId: area.id } : {};

  const status = await ok('POST', '/api/v1/statuses', { key: 'upg_live', name: 'Live (upgrade)' });
  const environment = await ok('POST', '/api/v1/environments', { key: 'upg_prod', name: 'Production (upgrade)' });
  const location = await ok('POST', '/api/v1/locations', { key: 'upg_dc1', name: 'Data centre 1 (upgrade)', locationType: 'site' });
  const owner = await ok('POST', '/api/v1/owners', { kind: 'team', name: 'Operations (upgrade)' });
  const list = await ok('POST', '/api/v1/lookup-lists', { key: 'upg_tier', name: 'Tier (upgrade)' });
  const gold = await ok('POST', '/api/v1/lookup-list-values', { listId: list.id, key: 'gold', name: 'Gold' });

  const cls = {
    server: await ok('POST', '/api/v1/ci-classes', { key: 'upg_server', name: 'Server (upgrade)', ...inArea }),
    application: await ok('POST', '/api/v1/ci-classes', { key: 'upg_application', name: 'Application (upgrade)', ...inArea }),
  };
  // One attribute per data type, with values that are easy to get subtly wrong
  // (fractions, time zones, IPv6, a CIDR, a lookup id, a reference).
  const attrDefs: Array<[string, string, Json]> = [
    ['serial_text', 'text', { validation: { maxLength: 100 } }],
    ['cpu_count', 'integer', { validation: { min: 1, max: 512 } }],
    ['ram_gb', 'number', { validation: { unit: 'GB' } }],
    ['is_virtual', 'boolean', {}],
    ['os_family', 'enum', { enumValues: ['linux', 'windows'] }],
    ['purchased_on', 'date', {}],
    ['patched_at', 'datetime', {}],
    ['mgmt_ip', 'ip', {}],
    ['subnet', 'cidr', {}],
    ['tier', 'lookup', { lookupListId: list.id }],
    ['primary_app', 'reference', { referenceClassId: cls.application.id }],
  ];
  const attributes: Record<string, Json> = {};
  for (const [key, dataType, extra] of attrDefs) {
    attributes[key] = await ok('POST', '/api/v1/attribute-definitions', { classId: cls.server.id, key, dataType, label: key.replace('_', ' '), ...extra });
  }
  attributes.version = await ok('POST', '/api/v1/attribute-definitions', { classId: cls.application.id, key: 'app_version', dataType: 'text', label: 'Version', isRequired: true });

  const app = await ok('POST', '/api/v1/configuration-items', {
    classId: cls.application.id, name: 'upg-app-01', statusId: status.id, ownerId: owner.id, attributes: { app_version: '4.2.1' },
  });
  const server = await ok('POST', '/api/v1/configuration-items', {
    classId: cls.server.id, name: 'upg-srv-01', statusId: status.id, environmentId: environment.id, locationId: location.id, ownerId: owner.id,
    hostname: 'upg-srv-01.example.internal', ipAddress: '10.20.30.40', serialNumber: 'SN-UPG-0001', notes: 'Seeded by the upgrade check.\nSecond line, ümlauts.',
    attributes: {
      serial_text: 'ABC-123', cpu_count: 16, ram_gb: 62.5, is_virtual: false, os_family: 'linux', purchased_on: '2024-02-29',
      patched_at: '2026-01-15T08:30:00.000Z', mgmt_ip: '2001:db8::10', subnet: '10.20.30.0/24', tier: gold.id, primary_app: app.id,
    },
  });
  const server2 = await ok('POST', '/api/v1/configuration-items', {
    classId: cls.server.id, name: 'upg-srv-02', statusId: status.id, ipAddress: '10.20.30.41', attributes: { cpu_count: 2, is_virtual: true },
  });
  // An edit and a delete, so the audit log has update and delete rows too.
  await ok('PATCH', `/api/v1/configuration-items/${server2.id}`, { notes: 'edited before the upgrade', attributes: { ram_gb: 8 } });
  const gone = await ok('POST', '/api/v1/configuration-items', { classId: cls.server.id, name: 'upg-srv-deleted', statusId: status.id });
  await ok('DELETE', `/api/v1/configuration-items/${gone.id}`);

  const relType = await ok('POST', '/api/v1/relationship-types', { key: 'upg_runs_on', name: 'Runs on (upgrade)', forwardLabel: 'runs on', reverseLabel: 'hosts' });
  const rule = await ok('POST', '/api/v1/relationship-rules', { relationshipTypeId: relType.id, sourceClassId: cls.application.id, targetClassId: cls.server.id });
  const rel = await ok('POST', '/api/v1/relationships', { relationshipTypeId: relType.id, sourceCiId: app.id, targetCiId: server.id, notes: 'seeded' });

  // A restricted profile: sees servers only, may not create them, no audit.view.
  const profile = await ok('POST', '/api/v1/admin/profiles', {
    name: 'Upgrade viewers', description: 'Server read-only (upgrade check)',
    classPermissions: [{ classId: cls.server.id, view: true, create: false, edit: false, delete: false }],
  });
  const viewer = await ok('POST', '/api/v1/admin/users', { username: VIEWER, displayName: 'Upgrade Viewer', password: VIEWER_PASSWORD, profileIds: [profile.id] });

  const ids = {
    area: area?.id ?? null,
    status: status.id, environment: environment.id, location: location.id, owner: owner.id, lookupList: list.id, lookupValue: gold.id,
    classes: { server: cls.server.id, application: cls.application.id },
    attributes: Object.fromEntries(Object.entries(attributes).map(([k, v]) => [k, v.id])),
    cis: { server: server.id, server2: server2.id, application: app.id },
    deletedCi: gone.id,
    relationshipType: relType.id, relationshipRule: rule.id, relationship: rel.id,
    profile: profile.id, viewer: viewer.id,
  };

  // The restricted user signs in once before the upgrade (a login.success row
  // in the audit log, and its view of the inventory to compare later).
  const viewerBefore = await viewerView(ids);
  me = await login(ADMIN, ADMIN_PASSWORD);
  const objects = await readObjects(ids);
  const inventory = await readInventory();
  const audit = await all('/api/v1/audit-log');
  const migrations = (await ok('GET', '/readyz')).migrations.applied;
  process.stdout.write(JSON.stringify({ migrations, ids, objects, inventory, audit, viewer: viewerBefore }, null, 1));
  console.error(`\nSnapshot: ${Object.keys(objects).length} objects, ${audit.length} audit rows, restricted user sees ${viewerBefore.visibleCiIds.length} CIs`);
}

/** The GET response of every seeded object, and of every CI, class and relationship in the lists, by URL. */
async function readObjects(ids: Json): Promise<Record<string, Json>> {
  const everything = [
    ...(await all('/api/v1/configuration-items')).map((c) => `/api/v1/configuration-items/${c.id}`),
    ...(await all('/api/v1/ci-classes')).flatMap((c) => [`/api/v1/ci-classes/${c.id}`, `/api/v1/ci-classes/${c.id}/attributes`]),
    ...(await all('/api/v1/relationships')).map((r) => `/api/v1/relationships/${r.id}`),
  ];
  const urls = [
    ids.area && `/api/v1/areas/${ids.area}`,
    `/api/v1/statuses/${ids.status}`, `/api/v1/environments/${ids.environment}`, `/api/v1/locations/${ids.location}`, `/api/v1/owners/${ids.owner}`,
    `/api/v1/lookup-lists/${ids.lookupList}`, `/api/v1/lookup-list-values/${ids.lookupValue}`,
    ...Object.values(ids.classes).flatMap((id) => [`/api/v1/ci-classes/${id}`, `/api/v1/ci-classes/${id}/attributes`]),
    ...Object.values(ids.attributes).map((id) => `/api/v1/attribute-definitions/${id}`),
    ...Object.values(ids.cis).flatMap((id) => [`/api/v1/configuration-items/${id}`, `/api/v1/configuration-items/${id}/graph`]),
    `/api/v1/relationship-types/${ids.relationshipType}`, `/api/v1/relationship-rules/${ids.relationshipRule}`,
    `/api/v1/relationships/${ids.relationship}`,
    `/api/v1/configuration-items/${ids.deletedCi}`,
    `/api/v1/admin/profiles/${ids.profile}`, `/api/v1/admin/users/${ids.viewer}`,
    ...everything,
  ].filter(Boolean) as string[];
  const out: Record<string, Json> = {};
  for (const url of new Set(urls)) out[url] = await ok('GET', url);
  return out;
}

/** What the lists show: active CIs (deleted ones drop out) and relationships. */
async function readInventory() {
  return {
    cis: (await all('/api/v1/configuration-items')).map((c) => c.id).sort(),
    relationships: (await all('/api/v1/relationships')).map((r) => r.id).sort(),
  };
}

// --- check: runs against the upgraded release ---

/**
 * Differences a migration makes on purpose. Each applies only when the old
 * database had fewer than `before` migrations applied (the migration that
 * causes it is number `before`, counting from 0000 as 1), so a later release
 * that does the same by accident still fails. Keep this list short: every
 * entry is a place where the check trusts the release notes instead of the data.
 */
const CHANGED_ON_PURPOSE: Array<{ url: RegExp; diff: RegExp; before: number; why: string }> = [
  {
    url: /^\/api\/v1\/ci-classes\/[^/]+$/, diff: /^\$\.updatedAt: /, before: 9,
    why: '0008 puts every existing class in the area "infrastruktur" with an UPDATE, which stamps updated_at',
  },
  {
    url: /^\/api\/v1\/ci-classes\/[^/]+\/attributes$/, diff: /^\$\.data: \d+ entries -> \d+$/, before: 17,
    why: '0016 adds the former fixed CI columns (name, status, ...) as attributes of the classes whose CIs held values',
  },
];

/** Migration 0016 (number 17) moved the fixed CI fields into class attributes. */
const CORE_CI_MODEL = 17;

/**
 * A CI as a release before 0016 returned it, in the shape 0016 gives it: the
 * name is the label and, like every other former fixed field, an attribute
 * (status, environment, owner and location by the same id, now a lookup list
 * value). The embedded status/environment/owner/location objects are gone;
 * their ids are compared through the attributes. A graph node or search item
 * (a summary, without attributes) keeps only the label.
 */
function asCoreModel(ci: Json, withAttributes: boolean): Json {
  const moved: Array<[string, string]> = [
    ['name', 'name'], ['statusId', 'status'], ['environmentId', 'environment'], ['ownerId', 'owner'], ['locationId', 'location'],
    ['hostname', 'hostname'], ['ipAddress', 'ip_address'], ['serialNumber', 'serial_number'], ['notes', 'notes'],
  ];
  const out: Json = { ...ci, label: ci.name };
  const attributes: Json = { ...(ci.attributes ?? {}) };
  for (const [field, key] of moved) {
    if (ci[field] !== null && ci[field] !== undefined) attributes[key] = ci[field];
    delete out[field];
  }
  for (const embedded of ['status', 'environment', 'owner', 'location']) delete out[embedded];
  if (withAttributes) out.attributes = attributes;
  return out;
}

/** The snapshot of a URL as the current release answers it, for data from before `CORE_CI_MODEL`. */
function translate(url: string, before: Json): Json {
  if (sourceMigrations >= CORE_CI_MODEL) return before;
  if (/^\/api\/v1\/configuration-items\/[^/]+$/.test(url)) return asCoreModel(before, true);
  if (/^\/api\/v1\/configuration-items\/[^/]+\/graph$/.test(url)) {
    return { ...before, nodes: before.nodes.map((n: Json) => asCoreModel(n, false)) };
  }
  return before;
}

/** Every value in `before` must be in `after` unchanged (after may have more). */
function diff(before: Json, after: Json, path: string, out: string[]): void {
  if (before === null || typeof before !== 'object') {
    if (before !== after) out.push(`${path}: ${JSON.stringify(before)} -> ${JSON.stringify(after)}`);
    return;
  }
  if (Array.isArray(before)) {
    if (!Array.isArray(after)) return void out.push(`${path}: was an array, now ${JSON.stringify(after)?.slice(0, 200)}`);
    if (before.length !== after.length) out.push(`${path}: ${before.length} entries -> ${after.length}`);
    // Lists of objects with ids are compared by id, whatever their order.
    const byId = before.every((x) => x && typeof x === 'object' && 'id' in x);
    before.forEach((b, i) => {
      const a = byId ? after.find((x: Json) => x?.id === b.id) : after[i];
      if (a === undefined) out.push(`${path}[${byId ? `id=${b.id}` : i}]: missing`);
      else diff(b, a, `${path}[${byId ? `id=${b.id}` : i}]`, out);
    });
    return;
  }
  if (after === null || typeof after !== 'object' || Array.isArray(after)) return void out.push(`${path}: was an object, now ${JSON.stringify(after)?.slice(0, 200)}`);
  for (const [k, v] of Object.entries(before)) {
    if (!(k in after)) out.push(`${path}.${k}: missing (was ${JSON.stringify(v)?.slice(0, 200)})`);
    else diff(v, after[k], `${path}.${k}`, out);
  }
}

function compare(label: string, url: string, before: Json, after: Json) {
  const found: string[] = [];
  diff(before, after, '$', found);
  const real = found.filter((f) => !CHANGED_ON_PURPOSE.some((c) => sourceMigrations < c.before && c.url.test(url) && c.diff.test(f)));
  for (const f of real) failures.push(`${label} ${url} ${f}`);
  console.error(`${real.length ? 'FAIL' : 'ok  '} ${label} ${url}${real.length ? `\n       ${real.slice(0, 10).join('\n       ')}` : ''}`);
}

let sourceMigrations = 0;

async function check() {
  let input = '';
  for await (const chunk of process.stdin) input += chunk;
  const snap = JSON.parse(input);
  const { ids } = snap;
  sourceMigrations = snap.migrations;

  // 1. The administrator can still sign in with the old password.
  me = await login(ADMIN, ADMIN_PASSWORD);

  // 2. Every object, field by field.
  for (const [url, before] of Object.entries(snap.objects)) {
    const r = await call('GET', url);
    if (r.status !== 200) failures.push(`GET ${url}: ${r.status}, expected 200`);
    else compare('object', url, translate(url, before), r.json);
  }
  // The same CIs and relationships in the lists: nothing lost, nothing deleted or restored.
  compare('lists', 'GET /api/v1/configuration-items, /api/v1/relationships', snap.inventory, await readInventory());

  // 3. The audit log: every row from before, unchanged, same order.
  const audit = await all('/api/v1/audit-log');
  const afterById = new Map(audit.map((a) => [a.id, a]));
  let auditDiffs = 0;
  for (const row of snap.audit) {
    const after = afterById.get(row.id);
    if (!after) {
      failures.push(`audit row ${row.id} (${row.action} ${row.entityType}) is gone`);
      auditDiffs++;
      continue;
    }
    const found: string[] = [];
    diff(row, after, `audit[${row.id}]`, found);
    for (const f of found) failures.push(f);
    auditDiffs += found.length;
  }
  console.error(`${auditDiffs ? 'FAIL' : 'ok  '} audit log: ${snap.audit.length} rows from before the upgrade, ${audit.length} now`);

  // 4. The restricted user signs in and sees exactly what they saw before.
  const view: ViewerView = await viewerView(ids);
  const found: string[] = [];
  // A CI in a hidden class answers 404 since #123 (403 before, an existence
  // oracle): both refuse it, so compare what was refused, not the status code.
  const refused = (v: ViewerView) => ({
    ...v,
    getStatus: Object.fromEntries(Object.entries(v.getStatus).map(([id, s]) => [id, s === 403 ? 404 : s])),
  });
  diff(refused(snap.viewer), refused(view), 'restricted user', found);
  for (const f of found) failures.push(f);
  console.error(`${found.length ? 'FAIL' : 'ok  '} restricted user: sees ${view.visibleCiIds.length} CIs, GET ${JSON.stringify(Object.values(view.getStatus))}, create ${view.createStatus}, audit ${view.auditStatus}`);

  if (failures.length) {
    console.error(`\n${failures.length} difference(s) after the upgrade:\n  ${failures.join('\n  ')}`);
    process.exit(1);
  }
  console.error(`\nUpgrade check passed: ${Object.keys(snap.objects).length} objects and ${snap.audit.length} audit rows unchanged, restricted user unchanged.`);
}

(MODE === 'seed' ? seed() : check()).catch((e) => {
  console.error(`\n${e instanceof Error ? e.message : e}`);
  process.exit(1);
});
