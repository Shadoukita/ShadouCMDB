#!/usr/bin/env node
/*
 * Semantic diff of two OpenAPI 3.1 documents: operations, parameters, request
 * bodies, responses and component schemas. Spellings that mean the same thing
 * are normalised first (e.g. `anyOf: [X, {type: null}]` vs `type: [x, "null"]`,
 * integer formats, zod's safe-integer bounds and format-implied patterns), so
 * what remains is a difference a client could notice.
 *
 *   node tools/openapi-diff.mjs old.json new.json
 *
 * Exit code 0 when the documents are equivalent, 1 otherwise.
 */
import { readFileSync } from 'node:fs';

const [oldPath, newPath] = process.argv.slice(2);
if (!oldPath || !newPath) {
  console.error('usage: node tools/openapi-diff.mjs <old.json> <new.json>');
  process.exit(2);
}
const load = (p) => JSON.parse(readFileSync(p, 'utf8'));
const A = load(oldPath);
const B = load(newPath);

const SAFE_INT = 9007199254740991;
const IMPLIED_PATTERN_FORMATS = new Set(['uuid', 'date-time', 'date', 'ipv4', 'ipv6', 'cidrv4', 'cidrv6', 'email']);

/** Canonical form of a schema (recursively). */
function norm(s) {
  if (Array.isArray(s)) return s.map(norm);
  if (!s || typeof s !== 'object') return s;
  let o = { ...s };

  // Nullable spellings -> { ...X, nullable: true }
  for (const k of ['anyOf', 'oneOf']) {
    if (Array.isArray(o[k])) {
      const nonNull = o[k].filter((b) => !(b && b.type === 'null' && Object.keys(b).length === 1));
      if (nonNull.length < o[k].length) {
        const rest = { ...o };
        delete rest[k];
        const inner = nonNull.length === 1 ? nonNull[0] : { [k]: nonNull };
        o = { ...rest, ...inner, nullable: true };
      }
    }
  }
  if (Array.isArray(o.type)) {
    const types = o.type.filter((t) => t !== 'null');
    if (types.length < o.type.length) o.nullable = true;
    o.type = types.length === 1 ? types[0] : [...types].sort();
  }
  if (Array.isArray(o.anyOf)) o = { ...o, anyOf: o.anyOf.map(norm) };
  if (Array.isArray(o.oneOf)) o = { anyOf: o.oneOf.map(norm), ...Object.fromEntries(Object.entries(o).filter(([k]) => k !== 'oneOf')) };

  if (o.format === 'int32' || o.format === 'int64') delete o.format;
  if (o.minimum === -SAFE_INT) delete o.minimum;
  if (o.maximum === SAFE_INT) delete o.maximum;
  if (o.pattern && IMPLIED_PATTERN_FORMATS.has(o.format)) delete o.pattern;
  if (o.type === 'integer' && o.minimum === 0 && o.maximum === undefined) delete o.minimum; // usize
  delete o.propertyNames;

  if (o.properties) o.properties = Object.fromEntries(Object.entries(o.properties).map(([k, v]) => [k, norm(v)]));
  if (o.items) o.items = norm(o.items);
  if (o.additionalProperties && typeof o.additionalProperties === 'object') o.additionalProperties = norm(o.additionalProperties);
  if (Array.isArray(o.required)) o.required = [...o.required].sort();
  if (Array.isArray(o.required) && o.required.length === 0) delete o.required;
  if (Array.isArray(o.enum)) o.enum = [...o.enum];
  return o;
}

const diffs = [];
function cmp(path, a, b) {
  if (a === b) return;
  if (typeof a !== typeof b || a === null || b === null || typeof a !== 'object') {
    diffs.push(`${path}: ${JSON.stringify(a)} -> ${JSON.stringify(b)}`);
    return;
  }
  if (Array.isArray(a) || Array.isArray(b)) {
    if (JSON.stringify(a) !== JSON.stringify(b)) diffs.push(`${path}: ${JSON.stringify(a)} -> ${JSON.stringify(b)}`);
    return;
  }
  for (const k of new Set([...Object.keys(a), ...Object.keys(b)])) {
    if (!(k in a)) diffs.push(`${path}.${k}: (absent) -> ${JSON.stringify(b[k])}`);
    else if (!(k in b)) diffs.push(`${path}.${k}: ${JSON.stringify(a[k])} -> (absent)`);
    else cmp(`${path}.${k}`, a[k], b[k]);
  }
}

// --- Document level -----------------------------------------------------------
cmp('openapi', A.openapi, B.openapi);
cmp('info', A.info, B.info);
cmp('servers', A.servers, B.servers);
cmp('security', A.security, B.security);
cmp('tags', A.tags, B.tags);

// --- Operations ---------------------------------------------------------------
const ops = (doc) =>
  Object.fromEntries(
    Object.entries(doc.paths).flatMap(([p, methods]) => Object.entries(methods).map(([m, op]) => [`${m.toUpperCase()} ${p}`, op])),
  );
const opsA = ops(A);
const opsB = ops(B);
for (const key of new Set([...Object.keys(opsA), ...Object.keys(opsB)])) {
  const a = opsA[key];
  const b = opsB[key];
  if (!a || !b) {
    diffs.push(`${key}: ${a ? 'removed' : 'added'}`);
    continue;
  }
  for (const f of ['operationId', 'tags', 'summary', 'description']) cmp(`${key} ${f}`, a[f], b[f]);
  const params = (op) =>
    Object.fromEntries((op.parameters ?? []).map((p) => [`${p.in}:${p.name}`, { ...p, schema: norm(p.schema) }]));
  cmp(`${key} parameters`, params(a), params(b));
  const pa = (a.parameters ?? []).map((p) => `${p.in}:${p.name}`).join(',');
  const pb = (b.parameters ?? []).map((p) => `${p.in}:${p.name}`).join(',');
  if (pa !== pb) diffs.push(`${key} parameter order: ${pa} -> ${pb}`);
  cmp(`${key} requestBody`, a.requestBody && { ...a.requestBody, content: { 'application/json': { schema: norm(a.requestBody.content['application/json'].schema) } } },
    b.requestBody && { ...b.requestBody, content: { 'application/json': { schema: norm(b.requestBody.content['application/json'].schema) } } });
  cmp(`${key} responses`, a.responses, b.responses);
}

// --- Components ---------------------------------------------------------------
const compA = A.components?.schemas ?? {};
const compB = B.components?.schemas ?? {};
for (const name of new Set([...Object.keys(compA), ...Object.keys(compB)])) {
  if (!compA[name] || !compB[name]) {
    diffs.push(`components.${name}: ${compA[name] ? 'removed' : 'added'}`);
    continue;
  }
  cmp(`components.${name}`, norm(compA[name]), norm(compB[name]));
}

console.log(`${Object.keys(opsA).length} -> ${Object.keys(opsB).length} operations, ${Object.keys(compA).length} -> ${Object.keys(compB).length} component schemas`);
if (diffs.length === 0) {
  console.log('No semantic differences.');
  process.exit(0);
}
console.log(`${diffs.length} difference(s):`);
for (const d of diffs) console.log(`- ${d}`);
process.exit(1);
