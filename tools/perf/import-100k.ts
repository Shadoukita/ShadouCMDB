/*
 * Bulk import performance check (spec SHAA-714 §7.3): 100,000 rows × 20
 * columns, with 2 lookup columns, 1 reference to pre-seeded CIs and 1
 * relationship column, through analysis, dry run and commit.
 *
 *   API_URL=http://localhost:3000 node tools/perf/import-100k.ts   # Node.js 22.18+, no dependencies
 *
 * It measures, and compares with the §3.6 targets:
 *   - analysis, dry run and commit durations, and commit rows/s;
 *   - the server's peak RSS increase (VmHWM − VmRSS before), when SERVER_PID
 *     names the server process on this host;
 *   - the WAL written by the commit, when PERF_WAL_CMD is a shell command that
 *     prints the result of `SELECT pg_current_wal_lsn()`, for example
 *     PERF_WAL_CMD='psql "$ADMIN_DATABASE_URL" -Atc "select pg_current_wal_lsn()"';
 *   - `GET /configuration-items` p95 latency while idle and during the commit
 *     (a parallel probe at 2 requests/s). Idle is measured before and after the
 *     commit, and the commit is compared with the larger of the two, because the
 *     list is slower on the grown inventory even without a concurrent commit.
 * Then it imports the same file again, which must end with 100,000 unchanged
 * rows and no per-CI audit entries, and analyses and dry-runs the same data as
 * an XLSX workbook (skip with --no-xlsx).
 *
 * The API must be connected to a real, migrated PostgreSQL seeded with
 * `shadoucmdb seed --demo`. The script creates two classes, one relationship
 * type and 100,100 CIs with a unique run suffix (PERF_RUN), so use a scratch
 * database. The targets assume the reference setup of §3.6 (4 vCPU / 8 GiB
 * server, PostgreSQL 16 on a separate 4 vCPU host) and a release build.
 *
 * Signing in: on a database without users the script completes first-run
 * setup with SETUP_TOKEN. Otherwise set PERF_USERNAME and PERF_PASSWORD to an
 * account holding the Administrator profile. Options: --rows=N (default
 * 100000, for a quick trial), --no-xlsx, --report-only (exit 0 even when a
 * target is missed). PERF_OUT=<file> also writes the results as JSON.
 */

// Built-ins without `import`, so Node runs this .ts file as a script (like tools/smoke).
const { execSync } = process.getBuiltinModule('node:child_process');
const { readFileSync, writeFileSync } = process.getBuiltinModule('node:fs');
const { crc32, deflateRawSync } = process.getBuiltinModule('node:zlib');

const BASE = (process.env.API_URL ?? '').replace(/\/$/, '');
if (!BASE) {
  console.error('Set API_URL, e.g. API_URL=http://localhost:3000');
  process.exit(2);
}
const arg = (name: string) => process.argv.find((a) => a.startsWith(`--${name}=`))?.split('=')[1];
const ROWS = Number(arg('rows') ?? 100_000);
const XLSX = !process.argv.includes('--no-xlsx');
const REPORT_ONLY = process.argv.includes('--report-only');
const RUN = process.env.PERF_RUN ?? Date.now().toString(36);
const USERNAME = process.env.PERF_USERNAME ?? 'perf-admin';
const PASSWORD = process.env.PERF_PASSWORD ?? `perf-${RUN}-password`;
const SETUP_TOKEN = process.env.SETUP_TOKEN ?? '';
const SERVER_PID = process.env.SERVER_PID ?? '';
const WAL_CMD = process.env.PERF_WAL_CMD ?? '';
const RACKS = 100;

type Json = any; // eslint-disable-line @typescript-eslint/no-explicit-any

let cookie = '';
let csrf = '';

async function call(method: string, url: string, body?: unknown, headers: Record<string, string> = {}): Promise<{ status: number; json: Json; ms: number }> {
  const started = performance.now();
  const res = await fetch(BASE + url, {
    method,
    headers: {
      ...(cookie ? { cookie } : {}),
      // Also on GETs: the job read is audited for an administrator and needs the token (GH#503).
      ...(csrf ? { 'x-csrf-token': csrf } : {}),
      ...(body !== undefined && !(body instanceof Uint8Array) ? { 'content-type': 'application/json' } : {}),
      ...headers,
    },
    body: body === undefined ? undefined : body instanceof Uint8Array ? body : JSON.stringify(body),
  });
  const text = await res.text();
  const ms = performance.now() - started;
  if (method === 'POST' && (url === '/api/v1/auth/login' || url === '/api/v1/setup')) {
    const session = res.headers.getSetCookie().map((c) => c.split(';')[0]!).find((c) => c.startsWith('shadoucmdb_session='));
    if (session) cookie = session;
  }
  const json = text && (res.headers.get('content-type') ?? '').includes('json') ? JSON.parse(text) : undefined;
  if (res.status >= 400) throw new Error(`${method} ${url} -> ${res.status} ${text.slice(0, 400)}`);
  return { status: res.status, json, ms };
}
const get = (url: string) => call('GET', url).then((r) => r.json);
const post = (url: string, body?: unknown) => call('POST', url, body).then((r) => r.json);

async function idByKey(collection: string, key: string): Promise<string> {
  const row = (await get(`/api/v1/${collection}?limit=200&q=${key}`)).data.find((r: Json) => r.key === key);
  if (!row) throw new Error(`seed row ${collection}/${key} not found; run \`shadoucmdb seed --demo\` first`);
  return row.id;
}

async function signIn() {
  if ((await get('/api/v1/setup')).setupRequired) {
    const json = await post('/api/v1/setup', { username: USERNAME, email: `${USERNAME}@example.com`, displayName: 'Performance check', password: PASSWORD, setupToken: SETUP_TOKEN });
    csrf = json.csrfToken;
  } else {
    csrf = (await post('/api/v1/auth/login', { username: USERNAME, password: PASSWORD })).csrfToken;
  }
}

// --- Data ------------------------------------------------------------------------

const pad = (n: number) => String(n).padStart(6, '0');
const rackName = (n: number) => `perf-${RUN}-rack-${String(n).padStart(3, '0')}`;
// Keys and labels carry a prefix so they never clash with the demo seed's inherited hardware fields.
const EXTRA: { key: string; label: string; dataType: string; value: (i: number) => string }[] = [
  { key: 'p_serial', label: 'Perf Serial number', dataType: 'text', value: (i) => `SN-${RUN}-${pad(i)}` },
  { key: 'p_notes', label: 'Perf Notes', dataType: 'text', value: (i) => `Host ${i} in row ${i % 40} of the perf hall, managed by team ${i % 17}` },
  { key: 'p_vendor', label: 'Perf Vendor', dataType: 'text', value: (i) => ['Acme', 'Globex', 'Initech', 'Umbrella'][i % 4]! },
  { key: 'p_model_no', label: 'Perf Model number', dataType: 'text', value: (i) => `M${(i * 7) % 900 + 100}` },
  { key: 'p_asset_tag', label: 'Perf Asset tag', dataType: 'text', value: (i) => `AT${pad(i * 3)}` },
  { key: 'p_contact', label: 'Perf Contact', dataType: 'text', value: (i) => `ops${i % 50}@example.internal` },
  { key: 'p_cpu_count', label: 'Perf CPU count', dataType: 'integer', value: (i) => String(2 ** (i % 6)) },
  { key: 'p_ram_gb', label: 'Perf RAM (GB)', dataType: 'integer', value: (i) => String(8 * ((i % 32) + 1)) },
  { key: 'p_disk_count', label: 'Perf Disks', dataType: 'integer', value: (i) => String(i % 12) },
  { key: 'p_power_kw', label: 'Perf Power (kW)', dataType: 'number', value: (i) => ((i % 400) / 100 + 0.25).toFixed(2) },
  { key: 'p_weight_kg', label: 'Perf Weight (kg)', dataType: 'number', value: (i) => ((i % 90) + 10.5).toFixed(1) },
  { key: 'p_installed', label: 'Perf Installed on', dataType: 'date', value: (i) => new Date(Date.UTC(2020, 0, 1) + (i % 2000) * 86_400_000).toISOString().slice(0, 10) },
  { key: 'p_mgmt_ip', label: 'Perf Management IP', dataType: 'ip', value: (i) => `10.${(i >> 16) & 255}.${(i >> 8) & 255}.${i & 255}` },
  { key: 'p_monitored', label: 'Perf Monitored', dataType: 'boolean', value: (i) => (i % 3 === 0 ? 'false' : 'true') },
  { key: 'p_os_version', label: 'Perf OS version', dataType: 'text', value: (i) => `Debian 1${i % 4}` },
];
const HEADERS = ['Name', 'Status', 'Environment', 'Rack', 'Uses', ...EXTRA.map((e) => e.label)];

function rowsOf(status: string[], environment: string[]): string[][] {
  const out: string[][] = [];
  for (let i = 1; i <= ROWS; i++) {
    out.push([
      `perf-${RUN}-host-${pad(i)}`,
      status[i % status.length]!,
      environment[i % environment.length]!,
      rackName(i % RACKS),
      rackName((i * 7 + 3) % RACKS),
      ...EXTRA.map((e) => e.value(i)),
    ]);
  }
  return out;
}

const csvField = (s: string) => (/[",\r\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s);
function csv(rows: string[][]): Uint8Array {
  return new TextEncoder().encode([HEADERS, ...rows].map((r) => r.map(csvField).join(',')).join('\r\n') + '\r\n');
}

const xmlEscape = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
function column(n: number): string {
  let s = '';
  for (n += 1; n > 0; n = Math.floor((n - 1) / 26)) s = String.fromCharCode(65 + ((n - 1) % 26)) + s;
  return s;
}

/** A minimal workbook: one sheet of inline strings and numbers. */
function xlsx(rows: string[][]): Uint8Array {
  const numeric = new Set(EXTRA.filter((e) => e.dataType === 'integer' || e.dataType === 'number').map((e) => HEADERS.indexOf(e.label)));
  const sheet: string[] = ['<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>'];
  [HEADERS, ...rows].forEach((row, r) => {
    const cells = row.map((v, c) => {
      const ref = `${column(c)}${r + 1}`;
      return r > 0 && numeric.has(c) ? `<c r="${ref}"><v>${v}</v></c>` : `<c r="${ref}" t="inlineStr"><is><t>${xmlEscape(v)}</t></is></c>`;
    });
    sheet.push(`<row r="${r + 1}">${cells.join('')}</row>`);
  });
  sheet.push('</sheetData></worksheet>');
  const ct = 'application/vnd.openxmlformats-officedocument.spreadsheetml';
  const rel = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships';
  return zip([
    ['[Content_Types].xml', `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="${ct}.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="${ct}.worksheet+xml"/></Types>`],
    ['_rels/.rels', `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="${rel}/officeDocument" Target="xl/workbook.xml"/></Relationships>`],
    ['xl/workbook.xml', `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="${rel}"><sheets><sheet name="Hosts" sheetId="1" r:id="rId1"/></sheets></workbook>`],
    ['xl/_rels/workbook.xml.rels', `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="${rel}/worksheet" Target="worksheets/sheet1.xml"/></Relationships>`],
    ['xl/worksheets/sheet1.xml', sheet.join('')],
  ]);
}

/** A ZIP archive of deflated entries (no data descriptors, no ZIP64). */
function zip(entries: [string, string][]): Uint8Array {
  const local: Buffer[] = [];
  const central: Buffer[] = [];
  let offset = 0;
  for (const [name, text] of entries) {
    const data = Buffer.from(text, 'utf8');
    const packed = deflateRawSync(data, { level: 6 });
    const nameBytes = Buffer.from(name, 'utf8');
    const crc = crc32(data);
    const head = Buffer.alloc(30);
    head.writeUInt32LE(0x04034b50, 0);
    head.writeUInt16LE(20, 4);
    head.writeUInt16LE(0x0800, 6); // UTF-8 names
    head.writeUInt16LE(8, 8); // deflate
    head.writeUInt32LE(crc, 14);
    head.writeUInt32LE(packed.length, 18);
    head.writeUInt32LE(data.length, 22);
    head.writeUInt16LE(nameBytes.length, 26);
    const dir = Buffer.alloc(46);
    dir.writeUInt32LE(0x02014b50, 0);
    dir.writeUInt16LE(20, 4);
    dir.writeUInt16LE(20, 6);
    dir.writeUInt16LE(0x0800, 8);
    dir.writeUInt16LE(8, 10);
    dir.writeUInt32LE(crc, 16);
    dir.writeUInt32LE(packed.length, 20);
    dir.writeUInt32LE(data.length, 24);
    dir.writeUInt16LE(nameBytes.length, 28);
    dir.writeUInt32LE(offset, 42);
    local.push(head, nameBytes, packed);
    central.push(dir, nameBytes);
    offset += head.length + nameBytes.length + packed.length;
  }
  const cd = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(cd.length, 12);
  end.writeUInt32LE(offset, 16);
  return new Uint8Array(Buffer.concat([...local, cd, end]));
}

// --- Measurements ------------------------------------------------------------------

function procStatus(field: 'VmRSS' | 'VmHWM'): number | null {
  if (!SERVER_PID) return null;
  const m = readFileSync(`/proc/${SERVER_PID}/status`, 'utf8').match(new RegExp(`^${field}:\\s+(\\d+) kB`, 'm'));
  return m ? Number(m[1]) * 1024 : null;
}

/** Resets VmHWM to the current RSS, so the peak below is this run's own. */
function resetPeak() {
  if (!SERVER_PID) return;
  try {
    writeFileSync(`/proc/${SERVER_PID}/clear_refs`, '5');
  } catch {
    console.log('note: could not reset VmHWM (needs the same user as the server); the peak includes earlier activity');
  }
}

function walLsn(): bigint | null {
  if (!WAL_CMD) return null;
  const m = execSync(WAL_CMD, { encoding: 'utf8' }).match(/([0-9A-F]+)\/([0-9A-F]+)/i);
  if (!m) throw new Error(`PERF_WAL_CMD printed no LSN`);
  return (BigInt(`0x${m[1]}`) << 32n) + BigInt(`0x${m[2]}`);
}

const p95 = (xs: number[]) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length ? s[Math.min(s.length - 1, Math.ceil(s.length * 0.95) - 1)]! : NaN;
};

/** `GET /configuration-items` twice a second until `stop()`; returns the latencies. */
function probe() {
  const latencies: number[] = [];
  let running = true;
  const done = (async () => {
    while (running) {
      const next = performance.now() + 500;
      latencies.push((await call('GET', '/api/v1/configuration-items?limit=50&sort=-updatedAt')).ms);
      await new Promise((r) => setTimeout(r, Math.max(0, next - performance.now())));
    }
  })();
  return { stop: async () => ((running = false), await done, latencies) };
}

async function settle(id: string, from: string[]): Promise<Json> {
  for (;;) {
    const job = await get(`/api/v1/imports/${id}`);
    if (!from.includes(job.status)) return job;
    await new Promise((r) => setTimeout(r, 250));
  }
}

const secs = (ms: number) => ms / 1000;
const t0 = () => performance.now();

/** Upload, analyse, map and dry-run one file; returns the job and the durations. */
async function prepare(bytes: Uint8Array, format: 'csv' | 'xlsx', mapping: Json) {
  const contentType = format === 'csv' ? 'text/csv' : 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet';
  let t = t0();
  const job = (await call('POST', '/api/v1/imports', bytes, { 'content-type': contentType, 'x-file-name': `perf-${RUN}.${format}` })).json;
  const upload = performance.now() - t;
  t = t0();
  const analysed = await settle(job.id, ['uploading', 'queued', 'analysing']);
  const analysis = performance.now() - t;
  if (analysed.status !== 'ready' || analysed.file.rowCount !== ROWS) throw new Error(`analysis ended ${analysed.status} with ${analysed.file?.rowCount} rows: ${JSON.stringify(analysed.error)}`);
  await call('PUT', `/api/v1/imports/${job.id}/mapping`, mapping);
  t = t0();
  await post(`/api/v1/imports/${job.id}/dry-run`);
  const validated = await settle(job.id, ['queued', 'validating']);
  const dryRun = performance.now() - t;
  if (validated.status !== 'validated') throw new Error(`dry run ended ${validated.status}: ${JSON.stringify(validated.error)}`);
  if (validated.summary.errorRows) throw new Error(`dry run found ${validated.summary.errorRows} error rows`);
  return { id: job.id as string, summary: validated.summary, upload, analysis, dryRun };
}

async function commit(id: string) {
  const t = t0();
  await post(`/api/v1/imports/${id}/commit`, { skipErrorRows: false });
  const job = await settle(id, ['queued', 'committing']);
  const ms = performance.now() - t;
  if (job.status !== 'completed') throw new Error(`commit ended ${job.status}: ${JSON.stringify(job.error)}`);
  return { job, ms };
}

const auditCount = async (id: string, entityType: string) =>
  (await get(`/api/v1/audit-log?requestId=import:${id}&entityType=${entityType}&limit=1`)).page.total as number;

// --- Run ------------------------------------------------------------------------

async function main() {
  await signIn();
  await call('PUT', '/api/v1/imports/settings', { enabled: true });

  // The model: a rack class with pre-seeded CIs, and a host class with 20 importable columns.
  console.log(`# setup (run ${RUN})`);
  const hardware = await idByKey('ci-classes', 'hardware');
  const infra = await idByKey('areas', 'infrastruktur');
  const valueKeys = async (list: string) => (await get(`/api/v1/lookup-list-values?listId=${await idByKey('lookup-lists', list)}&limit=200`)).data
    .filter((v: Json) => v.isActive).map((v: Json) => ({ id: v.id as string, key: v.key as string }));
  const status = (await valueKeys('status')).slice(0, 3);
  const environment = (await valueKeys('environment')).slice(0, 3);
  const rackClass = await post('/api/v1/ci-classes', { key: `perf_rack_${RUN}`, name: `Perf rack ${RUN}`, parentId: hardware, areaId: infra });
  const hostClass = await post('/api/v1/ci-classes', { key: `perf_host_${RUN}`, name: `Perf host ${RUN}`, parentId: hardware, areaId: infra });
  await post('/api/v1/attribute-definitions', { classId: hostClass.id, key: 'p_rack', label: 'Perf rack', dataType: 'reference', referenceClassId: rackClass.id });
  for (const e of EXTRA) await post('/api/v1/attribute-definitions', { classId: hostClass.id, key: e.key, label: e.label, dataType: e.dataType });
  const uses = await post('/api/v1/relationship-types', { key: `perf_uses_${RUN}`, name: 'Uses', forwardLabel: 'uses', reverseLabel: 'is used by' });
  await post('/api/v1/relationship-rules', { relationshipTypeId: uses.id, sourceClassId: hostClass.id, targetClassId: rackClass.id });
  for (let n = 0; n < RACKS; n++) {
    await post('/api/v1/configuration-items', { classId: rackClass.id, attributes: { name: rackName(n), status: status[0]!.id, environment: environment[0]!.id } });
  }

  const byName = { by: 'attribute', attributeKey: 'name' };
  const mapping = {
    classKey: hostClass.key,
    mode: 'create_or_update',
    key: { field: 'attributes.name' },
    columns: [
      { index: 0, target: { kind: 'attribute', key: 'name' } },
      { index: 1, target: { kind: 'attribute', key: 'status' } },
      { index: 2, target: { kind: 'attribute', key: 'environment' } },
      { index: 3, target: { kind: 'attribute', key: 'p_rack', match: byName } },
      { index: 4, target: { kind: 'relationship', typeKey: uses.key, direction: 'outgoing', match: byName } },
      ...EXTRA.map((e, i) => ({ index: 5 + i, target: { kind: 'attribute', key: e.key } })),
    ],
  };

  let t = t0();
  const rows = rowsOf(status.map((v) => v.key), environment.map((v) => v.key));
  const csvBytes = csv(rows);
  console.log(`generated ${ROWS} rows × ${HEADERS.length} columns: CSV ${(csvBytes.length / 1048576).toFixed(1)} MiB in ${secs(performance.now() - t).toFixed(1)} s`);

  // Idle latency baseline: 20 requests at 2/s.
  const idleProbe = probe();
  await new Promise((r) => setTimeout(r, 10_000));
  const idle = await idleProbe.stop();

  // First run: creates every host.
  const rssBefore = procStatus('VmRSS');
  resetPeak();
  console.log('# first import (CSV)');
  const first = await prepare(csvBytes, 'csv', mapping);
  console.log(`upload ${secs(first.upload).toFixed(1)} s, analysis ${secs(first.analysis).toFixed(1)} s, dry run ${secs(first.dryRun).toFixed(1)} s: ${JSON.stringify(first.summary)}`);
  const walBefore = walLsn();
  const busyProbe = probe();
  const committed = await commit(first.id);
  const busy = await busyProbe.stop();
  const walAfter = walLsn();
  const peak = procStatus('VmHWM');
  const counts = committed.job.summary.committed;
  console.log(`commit ${secs(committed.ms).toFixed(1)} s: ${JSON.stringify(counts)}`);

  // Idle again, now with the imported CIs in the inventory: the list itself is
  // slower on the larger table, so this separates growth from contention.
  const afterProbe = probe();
  await new Promise((r) => setTimeout(r, 10_000));
  const idleAfter = await afterProbe.stop();

  // Second run of the same file: nothing changes, nothing is audited per CI.
  console.log('# second import of the same file');
  const second = await prepare(csvBytes, 'csv', mapping);
  const again = await commit(second.id);
  const againCounts = again.job.summary.committed;
  const againAudit = (await auditCount(second.id, 'configuration_items')) + (await auditCount(second.id, 'ci_relationships'));
  console.log(`dry run ${JSON.stringify(second.summary)}; commit ${secs(again.ms).toFixed(1)} s: ${JSON.stringify(againCounts)}; per-CI audit entries ${againAudit}`);

  // The same data as a workbook: analysis and dry run.
  let book: Awaited<ReturnType<typeof prepare>> | null = null;
  if (XLSX) {
    t = t0();
    const bytes = xlsx(rows);
    console.log(`# XLSX (${(bytes.length / 1048576).toFixed(1)} MiB, built in ${secs(performance.now() - t).toFixed(1)} s)`);
    book = await prepare(bytes, 'xlsx', mapping);
    console.log(`upload ${secs(book.upload).toFixed(1)} s, analysis ${secs(book.analysis).toFixed(1)} s, dry run ${secs(book.dryRun).toFixed(1)} s: ${JSON.stringify(book.summary)}`);
    await post(`/api/v1/imports/${book.id}/cancel`);
  }
  const peakAll = procStatus('VmHWM');

  // --- Report ----------------------------------------------------------------------
  const scale = ROWS / 100_000;
  const results: { check: string; value: string; target: string; pass: boolean | null }[] = [];
  const row = (check: string, value: string, target: string, pass: boolean | null) => results.push({ check, value, target, pass });
  row('CSV analysis', `${secs(first.analysis).toFixed(1)} s`, '≤ 60 s', secs(first.analysis) <= 60 * scale);
  row('CSV dry run', `${secs(first.dryRun).toFixed(1)} s`, '≤ 180 s', secs(first.dryRun) <= 180 * scale);
  row('Commit', `${secs(committed.ms).toFixed(1)} s (${(ROWS / secs(committed.ms)).toFixed(0)} rows/s)`, '≤ 600 s, ≥ 170 rows/s', secs(committed.ms) <= 600 * scale);
  row('Commit counts', `${counts.created} created, ${counts.relationshipsAdded} relationships`, `${ROWS} and ${ROWS}`, counts.created === ROWS && counts.relationshipsAdded === ROWS && counts.failed === 0);
  if (book) {
    row('XLSX analysis', `${secs(book.analysis).toFixed(1)} s`, '≤ 60 s', secs(book.analysis) <= 60 * scale);
    row('XLSX dry run', `${secs(book.dryRun).toFixed(1)} s`, '≤ 180 s', secs(book.dryRun) <= 180 * scale);
    row('XLSX dry run result', `${book.summary.unchanged} unchanged`, `${ROWS}`, book.summary.unchanged === ROWS);
  }
  const rssUp = rssBefore !== null && peakAll !== null ? peakAll - rssBefore : null;
  row('Peak RSS increase', rssUp === null ? 'not measured (set SERVER_PID)' : `${(rssUp / 1048576).toFixed(0)} MiB (commit phase ${((peak! - rssBefore!) / 1048576).toFixed(0)} MiB)`, '≤ 300 MiB', rssUp === null ? null : rssUp <= 300 * 1048576);
  row('WAL written by the commit', walBefore === null ? 'not measured (set PERF_WAL_CMD)' : `${(Number(walAfter! - walBefore) / 1048576).toFixed(0)} MiB`, 'recorded', null);
  const idleP95 = p95(idle);
  const busyP95 = p95(busy);
  const afterP95 = p95(idleAfter);
  row('GET /configuration-items p95', `idle ${idleP95.toFixed(0)} ms before, ${afterP95.toFixed(0)} ms after; during commit ${busyP95.toFixed(0)} ms (${busy.length} requests)`,
    '≤ 2× idle', busyP95 <= 2 * Math.max(idleP95, afterP95));
  row('Same file again', `${againCounts.unchanged} unchanged, ${againCounts.created + againCounts.updated} written, ${againAudit} per-CI audit entries`, `${ROWS} unchanged, 0, 0`,
    againCounts.unchanged === ROWS && againCounts.created + againCounts.updated === 0 && againAudit === 0);

  console.log(`\n${ROWS} rows × ${HEADERS.length} columns\n`);
  console.log('| Check | Result | Target | |\n|---|---|---|---|');
  for (const r of results) console.log(`| ${r.check} | ${r.value} | ${r.target} | ${r.pass === null ? '' : r.pass ? 'pass' : '**FAIL**'} |`);
  if (process.env.PERF_OUT) writeFileSync(process.env.PERF_OUT, JSON.stringify({ run: RUN, rows: ROWS, results }, null, 2));
  const failed = results.filter((r) => r.pass === false);
  if (failed.length && !REPORT_ONLY) process.exit(1);
}

main().catch((e) => {
  console.error(e instanceof Error ? e.message : e);
  process.exit(1);
});
