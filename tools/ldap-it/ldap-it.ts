/*
 * LDAP / Active Directory sign-in against a real directory: OpenLDAP over LDAPS
 * with a test CA (tools/ldap-it/ldap.sh), a real ShadouCMDB server and its
 * PostgreSQL. Covers directory sign-in and group mapping, the login throttle,
 * TLS trust and the connection test (GH#125), requireMfa for directory
 * accounts with the directory password as re-authentication (GH#120), and a
 * renamed directory entry.
 *
 *   tools/ldap-it/ldap.sh start && tools/ldap-it/ldap.sh seed
 *   API_URL=http://127.0.0.1:3003 node tools/ldap-it/ldap-it.ts   # Node.js 22.18+, no dependencies
 *
 * Needs a migrated database without users (the script completes first-run
 * setup), and runs tools/ldap-it/ldap.sh (same LDAP_IT_* variables) to stop,
 * start and change the directory, and `psql` (PG* variables of that
 * database) for the one state the API cannot reach. It changes the directory:
 * seed it again (`ldap.sh remove`, `start`, `seed`) before the next run.
 * README.md in this directory has the details.
 */

import { execFileSync } from 'node:child_process';
import { createHmac } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const BASE = (process.env.API_URL ?? '').replace(/\/$/, '');
if (!BASE) {
  console.error('Set API_URL, e.g. API_URL=http://127.0.0.1:3003');
  process.exit(2);
}
const HERE = import.meta.dirname;
const LDAP_DIR = process.env.LDAP_IT_DIR ?? join(process.env.RUNNER_TEMP ?? process.env.TMPDIR ?? '/tmp', 'ldap-it');
const PORT = process.env.LDAP_IT_PORT ?? '6360';
const LDAP_URL = `ldaps://127.0.0.1:${PORT}`;
/** The same directory under a name its certificate does not carry. */
const WRONG_NAME_URL = `ldaps://127.0.0.2:${PORT}`;
/** Nothing listens on port 1. */
const CLOSED_URL = 'ldaps://127.0.0.1:1';
const CA = readFileSync(join(LDAP_DIR, 'certs/ca.pem'), 'utf8');
const OTHER_CA = readFileSync(join(LDAP_DIR, 'certs/other-ca.pem'), 'utf8');
const SERVE_LOG = process.env.LDAP_IT_SERVE_LOG;
/** The server's first-run setup token (its SETUP_TOKEN, GitHub #192). */
const SETUP_TOKEN = process.env.SETUP_TOKEN ?? '';
if (!SETUP_TOKEN) {
  console.error('Set SETUP_TOKEN to the first-run setup token the server runs with');
  process.exit(2);
}

const SUFFIX = 'dc=shadoucmdb,dc=test';
const ADMIN = 'ldap-it-admin';
const ADMIN_PASSWORD = process.env.LDAP_IT_ADMIN_PASSWORD ?? `ldap-it-${Date.now().toString(36)}-password`;
// The fixture's throwaway passwords (seed.ldif).
const ALICE = { username: 'alice', password: 'alice-ci-only-password' };
const BOB = { username: 'bob', password: 'bob-ci-only-password' };
const MAPPED_GROUP = `CN=cmdb-operators,OU=groups,${SUFFIX.toUpperCase()}`; // case differs from the directory on purpose
const UNMAPPED_GROUP = `cn=cmdb-guests,ou=groups,${SUFFIX}`;
const PROFILE = 'LDAP IT operators';

/** GH#125: all a failed connection test may say when no LDAP answer came back over verified TLS. */
const UNREACHABLE =
  'Could not reach the directory over verified TLS (no connection, TLS or StartTLS failed, or no LDAP answer); the server log has the details';
/** What answers must not contain: TLS and socket error details, addresses. */
const LEAK = /certificate|UnknownIssuer|NotValidForName|BadSignature|rustls|handshake|os error|refused|io error|127\.0\.0\.|ldaps:/i;

type Json = any; // eslint-disable-line @typescript-eslint/no-explicit-any

interface Identity {
  cookie: string;
  csrf: string;
}
interface Res {
  status: number;
  json: Json;
  headers: Headers;
}

const failures: string[] = [];
let checks = 0;

function check(ok: unknown, what: string, detail?: unknown): void {
  checks++;
  if (ok) {
    console.log(`  ok    ${what}`);
  } else {
    console.log(`  FAIL  ${what}${detail === undefined ? '' : `: ${JSON.stringify(detail).slice(0, 600)}`}`);
    failures.push(what);
  }
}

function section(title: string): void {
  console.log(`\n# ${title}`);
}

async function call(who: Identity | null, method: string, url: string, body?: unknown, cookie?: string): Promise<Res> {
  const cookies = [who?.cookie, cookie].filter(Boolean).join('; ');
  const res = await fetch(BASE + url, {
    method,
    headers: {
      ...(cookies ? { cookie: cookies } : {}),
      ...(who && method !== 'GET' ? { 'x-csrf-token': who.csrf } : {}),
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
  return { status: res.status, json, headers: res.headers };
}

/** A call that must succeed; returns the body. */
async function ok(who: Identity | null, method: string, url: string, body?: unknown): Promise<Json> {
  const r = await call(who, method, url, body);
  if (r.status >= 400) throw new Error(`${method} ${url}: ${r.status} ${JSON.stringify(r.json).slice(0, 600)}`);
  return r.json;
}

function cookieOf(r: Res, name: string): string | undefined {
  return r.headers.getSetCookie().map((c) => c.split(';')[0]!).find((c) => c.startsWith(`${name}=`) && c.length > name.length + 1);
}

function identity(r: Res): Identity {
  const cookie = cookieOf(r, 'shadoucmdb_session');
  if (!cookie) throw new Error(`no session cookie: ${r.status} ${JSON.stringify(r.json).slice(0, 300)}`);
  return { cookie, csrf: r.json?.csrfToken ?? '' };
}

/** The renewed session after an authenticator confirm (GH#510): the CSRF token comes from its cookie. */
function renewed(r: Res): Identity {
  const cookie = cookieOf(r, 'shadoucmdb_session');
  const csrf = cookieOf(r, 'shadoucmdb_csrf')?.slice('shadoucmdb_csrf='.length);
  if (!cookie || !csrf) throw new Error(`no renewed session cookies: ${r.status}`);
  return { cookie, csrf };
}

const login = (u: { username: string; password: string }) => call(null, 'POST', '/api/v1/auth/login', u);
const code = (r: Res) => r.json?.error?.code;
const message = (r: Res) => r.json?.error?.message ?? '';
const field = (r: Res) => r.json?.error?.details?.[0]?.field;
/** For failure output of the MFA routes: never the secret, recovery codes or session data. */
const brief = (r: Res) => ({ status: r.status, error: code(r), field: field(r) });
/** The error body without its `requestId`, which differs on every response. */
const errorBody = (r: Res) => JSON.stringify({ ...r.json, error: { ...r.json?.error, requestId: undefined } });
/** A refused sign-in must not tell a right password from a wrong one (GH#437). */
const sameAnswer = (a: Res, b: Res) => a.status === b.status && errorBody(a) === errorBody(b);

/** The `reason` of the newest `login.failure` row, if it is `username`'s. */
async function lastFailureReason(admin: Identity, username: string): Promise<string | undefined> {
  const last = (await ok(admin, 'GET', '/api/v1/audit-log?action=login.failure&sort=-occurredAt&limit=1')).data[0];
  return last?.newValue?.attemptedUsername === username ? last.newValue.reason : undefined;
}

/**
 * A sign-in that waits out the name's lock first. A 503 while the directory cannot be reached
 * counts as a failed sign-in (GH#586), so a run of them locks the name for a few seconds.
 */
async function loginAfterLock(u: { username: string; password: string }): Promise<Res> {
  let r = await login(u);
  for (let i = 0; i < 5 && r.status === 429; i++) {
    await new Promise((done) => setTimeout(done, Number(r.headers.get('retry-after') ?? 1) * 1000));
    r = await login(u);
  }
  return r;
}

/** Runs tools/ldap-it/ldap.sh with the same environment. */
function directory(command: string, stdin?: string): void {
  execFileSync(join(HERE, 'ldap.sh'), [command], { input: stdin, stdio: [stdin === undefined ? 'ignore' : 'pipe', 'inherit', 'inherit'] });
}

function sql(statement: string): string {
  return execFileSync(process.env.LDAP_IT_PSQL ?? 'psql', ['-XAtqv', 'ON_ERROR_STOP=1', '-c', statement], { encoding: 'utf8' }).trim();
}

// --- TOTP (RFC 6238: SHA-1, 6 digits, 30 s) ------------------------------------------

function base32(s: string): Buffer {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567';
  let bits = '';
  for (const c of s.replace(/=+$/, '').toUpperCase()) bits += alphabet.indexOf(c).toString(2).padStart(5, '0');
  const bytes = [];
  for (let i = 0; i + 8 <= bits.length; i += 8) bytes.push(parseInt(bits.slice(i, i + 8), 2));
  return Buffer.from(bytes);
}

function hotp(secret: Buffer, step: number): string {
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(step));
  const mac = createHmac('sha1', secret).update(counter).digest();
  const offset = mac[mac.length - 1]! & 0xf;
  return ((mac.readUInt32BE(offset) & 0x7fffffff) % 1_000_000).toString().padStart(6, '0');
}

/** Codes for successive steps: the server takes a step once, and only one step either side of now. */
function authenticator(secret: string): () => Promise<string> {
  const key = base32(secret);
  let last = -Infinity;
  return async () => {
    for (;;) {
      const now = Math.floor(Date.now() / 30_000);
      const step = Math.max(last + 1, now - 1);
      if (step <= now + 1) {
        last = step;
        return hotp(key, step);
      }
      await new Promise((r) => setTimeout(r, 1000));
    }
  };
}

// --- The run ----------------------------------------------------------------------------

async function main(): Promise<void> {
  section('Setup: administrator, profile, directory with one mapped group');
  const setup = await call(null, 'POST', '/api/v1/setup', { username: ADMIN, email: `${ADMIN}@example.com`, displayName: 'LDAP IT admin', password: ADMIN_PASSWORD, setupToken: SETUP_TOKEN });
  if (setup.status !== 201) throw new Error(`first-run setup: ${setup.status} ${JSON.stringify(setup.json)} (use an empty, migrated database)`);
  const admin = identity(setup);
  const profile = await ok(admin, 'POST', '/api/v1/admin/profiles', {
    name: PROFILE,
    description: 'Mapped from the directory group cmdb-operators',
    classPermissions: [{ classId: null, view: true, create: false, edit: false, delete: false }],
  });
  const ldap = {
    url: LDAP_URL,
    bindDn: `cn=cmdb-reader,ou=services,${SUFFIX}`,
    bindPassword: 'reader-ci-only-password',
    userBaseDn: `ou=people,${SUFFIX}`,
    userFilter: '(&(objectClass=inetOrgPerson)(uid={username}))',
    usernameAttribute: 'uid',
    displayNameAttribute: 'displayName',
    emailAttribute: 'mail',
    groupAttribute: 'memberOf',
  };
  const plain = await call(admin, 'POST', '/api/v1/admin/identity-providers', {
    kind: 'ldap',
    name: 'Plain LDAP',
    ldap: { ...ldap, url: `ldap://127.0.0.1:${PORT}`, startTls: false },
  });
  check(plain.status === 400 && field(plain) === 'ldap.startTls', 'plain LDAP (ldap:// without StartTLS) is refused: 400 ldap.startTls', plain.json);
  const provider = await ok(admin, 'POST', '/api/v1/admin/identity-providers', {
    kind: 'ldap',
    name: 'LDAP IT directory',
    caCertificate: CA,
    ldap,
    groupMappings: [{ group: MAPPED_GROUP, profileId: profile.id }],
  });
  const P = `/api/v1/admin/identity-providers/${provider.id}`;
  const patch = (body: unknown) => ok(admin, 'PATCH', P, body);
  const test = async (username?: string) => (await ok(admin, 'POST', `${P}/test`, username ? { username } : {})) as Json;
  const providers = await ok(null, 'GET', '/api/v1/auth/providers');
  check(providers.directory === true, 'GET /auth/providers: the password form also takes directory accounts', providers);

  // The bind password saved above is stored encrypted under ENCRYPTION_KEY_FILE (PR #237).
  // Settings are read from the row on every sign-in, so the directory sign-ins below only
  // work if the server decrypts it again; the Rust suite has no directory to prove that.
  const stored = await ok(admin, 'GET', P);
  check(stored.ldap?.bindPasswordSet === true && !JSON.stringify(stored).includes(ldap.bindPassword), 'the admin API reports the bind password as set, never its value', stored.ldap);
  const atRest = sql(
    `SELECT bind_password IS NULL, bind_password_enc IS NOT NULL, secrets_key_id IS NOT NULL,
            position(convert_to('${ldap.bindPassword}', 'UTF8') IN bind_password_enc) = 0
       FROM cmdb.identity_providers WHERE id = '${provider.id}'`,
  );
  check(atRest === 't|t|t|t', 'at rest: bind_password_enc and key id set, no plaintext column, no plaintext in the ciphertext', atRest);

  // ---------------------------------------------------------------------------------------
  section('1. Directory sign-in and group mapping');
  const aliceLogin = await login(ALICE);
  check(
    aliceLogin.status === 200 && cookieOf(aliceLogin, 'shadoucmdb_session'),
    'alice signs in with her directory password (200, session cookie): the service bind used the decrypted bind password',
    brief(aliceLogin),
  );
  let alice = identity(aliceLogin);
  const me = await ok(alice, 'GET', '/api/v1/auth/me');
  check(me.user.username === 'alice' && me.user.identityProvider?.id === provider.id, 'the account is created and linked to the directory', me.user);
  check(me.user.displayName === 'Alice Directory' && me.user.email === 'alice@shadoucmdb.test', 'display name and e-mail come from the entry', me.user);
  check(
    JSON.stringify(me.user.profiles.map((p: Json) => p.name)) === JSON.stringify([PROFILE]),
    'the mapped group gives exactly its profile; the unmapped group gives nothing',
    me.user.profiles,
  );
  check(me.mfa.required === false && me.mfa.enrolmentRequired === false, 'no second factor required yet');
  const inventory = await call(alice, 'GET', '/api/v1/configuration-items?limit=1');
  check(inventory.status === 200, 'the profile applies: alice reads the inventory', inventory.json);
  const denied = await call(alice, 'GET', '/api/v1/admin/users');
  check(denied.status === 403, 'and nothing more: /admin/users is 403 for her', denied.json);

  const bobLogin = await login(BOB);
  const bobReason = await lastFailureReason(admin, 'bob');
  check(bobLogin.status === 401 && code(bobLogin) === 'UNAUTHENTICATED', 'bob (only in an unmapped group) is refused: 401', bobLogin.json);
  check(bobReason === 'not_authorised', 'the audit log keeps the reason: not_authorised', bobReason);
  const users = await ok(admin, 'GET', '/api/v1/admin/users?limit=100');
  check(!users.data.some((u: Json) => u.username === 'bob'), 'no account is created for bob', users.data.map((u: Json) => u.username));

  const wrong = await login({ username: 'alice', password: 'not-her-password' });
  const unknown = await login({ username: 'nobody-in-the-directory', password: 'not-her-password' });
  check(wrong.status === 401 && code(wrong) === 'UNAUTHENTICATED', 'a wrong directory password: 401 UNAUTHENTICATED', wrong.json);
  check(
    unknown.status === wrong.status && code(unknown) === code(wrong) && message(unknown) === message(wrong),
    'an unknown name gets the same answer as a wrong password',
    { wrong: wrong.json, unknown: unknown.json },
  );
  // Compared with alice's wrong password: another wrong one for bob would count toward his lock below.
  check(sameAnswer(bobLogin, wrong), "bob's refusal is the same answer as a wrong password", { refused: bobLogin.json, wrong: wrong.json });
  check(!cookieOf(wrong, 'shadoucmdb_session') && !cookieOf(unknown, 'shadoucmdb_session'), 'neither sets a session cookie');
  // GH#406: OpenLDAP's uid matching (RFC 4518 string preparation) finds alice's entry for other
  // spellings, each of which the per-name lock would count on its own. Only names an account could
  // have are looked up: a full-width spelling with her right password is an unknown name.
  const wide = await login({ username: 'ａｌｉｃｅ', password: ALICE.password });
  check(
    wide.status === 401 && message(wide) === message(wrong) && !cookieOf(wide, 'shadoucmdb_session'),
    'a full-width spelling of alice with her password is not looked up: the generic 401, no session',
    wide.json,
  );

  // Wrong directory passwords count toward the per-username lock (5 free failures, then 429).
  for (const [name, password] of [['bob', 'wrong-bob-password'], ['nobody-in-the-directory', 'still-wrong']]) {
    const answers: Res[] = [];
    for (let i = 0; i < 8; i++) {
      const r = await login({ username: name!, password: password! });
      answers.push(r);
      if (r.status === 429) break;
    }
    const last = answers[answers.length - 1]!;
    const before = answers.slice(0, -1);
    check(
      last.status === 429 && code(last) === 'RATE_LIMITED' && Number(last.headers.get('retry-after')) >= 1,
      `${name}: repeated wrong passwords lock the name (429 RATE_LIMITED with Retry-After)`,
      answers.map((r) => r.status),
    );
    check(
      before.length >= 4 && before.every((r) => r.status === 401 && message(r) === message(wrong)),
      `${name}: every attempt before the lock is the same generic 401`,
      answers.map((r) => r.status),
    );
    if (name === 'bob') {
      // Right away: the first lock lasts 1 s.
      const locked = await login(BOB);
      check(locked.status === 429, 'while locked, even the right directory password is answered 429', locked.json);
    }
  }

  // ---------------------------------------------------------------------------------------
  section('2. TLS trust and the connection test (GH#125)');
  let t = await test();
  check(t.ok === true && /service account bind succeeded/.test(t.message), 'test: TLS verified against the configured CA, service bind succeeds', t);
  t = await test('alice');
  check(
    t.ok === true &&
      t.user?.dn === `uid=alice,ou=people,${SUFFIX}` &&
      t.user.groups.length === 2 &&
      JSON.stringify(t.user.profiles) === JSON.stringify([PROFILE]),
    'test with alice: her entry, both groups, and the one profile they map to',
    t,
  );
  t = await test('bob');
  check(t.ok === false && JSON.stringify(t.user?.groups) === JSON.stringify([UNMAPPED_GROUP]) && t.user.profiles.length === 0, 'test with bob: found, but no group maps (ok false)', t);
  t = await test('nobody-in-the-directory');
  check(t.ok === false && t.user === null && /found no entry/.test(t.message), 'test with an unknown name: no entry', t);

  const untrusted = async (what: string, change: Json) => {
    await patch(change);
    const r = await test('alice');
    check(r.ok === false && r.message === UNREACHABLE && r.details.length === 0 && r.user === null, `${what}: the test answers the generic unreachable text`, r);
    check(!LEAK.test(JSON.stringify(r)), `${what}: the test leaks no TLS or socket detail`, r);
    const l = await loginAfterLock(ALICE);
    check(l.status === 503 && code(l) === 'IDENTITY_PROVIDER_UNAVAILABLE', `${what}: sign-in as alice is 503 IDENTITY_PROVIDER_UNAVAILABLE`, l.json);
    check(!LEAK.test(JSON.stringify(l.json)), `${what}: the sign-in error leaks no TLS or socket detail`, l.json);
    const reason = await lastFailureReason(admin, 'alice');
    check(reason === 'directory_unavailable', `${what}: the 503 is audited as login.failure, reason directory_unavailable (GH#586)`, reason);
    // GH#499: a name no account has gets the generic 401, so the outage does not tell local accounts apart.
    const nobody = await login({ username: 'nobody-else-in-the-directory', password: 'x-password' });
    check(nobody.status === 401 && code(nobody) === 'UNAUTHENTICATED', `${what}: sign-in as an unknown name is the generic 401`, nobody.json);
  };
  await untrusted('CA not trusted (another CA configured)', { caCertificate: OTHER_CA });
  await untrusted('CA not trusted (no CA configured)', { caCertificate: null });
  // GH#238: a new server address needs the bind password again; nothing changes without it.
  const moved = await call(admin, 'PATCH', P, { ldap: { url: WRONG_NAME_URL } });
  check(
    moved.status === 422 && code(moved) === 'SECRET_REQUIRED' && field(moved) === 'ldap.bindPassword',
    'moving the directory without its bind password: 422 SECRET_REQUIRED on ldap.bindPassword',
    moved.json,
  );
  check((await ok(admin, 'GET', P)).ldap.url === LDAP_URL, 'and the URL is unchanged');
  await untrusted('certificate not valid for the host name', { caCertificate: CA, ldap: { url: WRONG_NAME_URL, bindPassword: ldap.bindPassword } });
  await untrusted('closed port', { ldap: { url: CLOSED_URL, bindPassword: ldap.bindPassword } });
  if (SERVE_LOG) {
    const log = readFileSync(SERVE_LOG, 'utf8');
    check(/identity provider connection test failed/.test(log) && /UnknownIssuer|invalid peer certificate/i.test(log), 'the server log has the exact TLS error for the administrator');
  }
  await patch({ ldap: { url: LDAP_URL, bindPassword: 'wrong-service-password' } });
  t = await test();
  check(t.ok === false && /^service account bind: /.test(t.message), "a wrong service password: the directory's own answer is shown (it answered over verified TLS)", t);
  const noService = await loginAfterLock(ALICE);
  check(noService.status === 503 && code(noService) === 'IDENTITY_PROVIDER_UNAVAILABLE', 'and sign-in is 503 IDENTITY_PROVIDER_UNAVAILABLE', noService.json);
  await patch({ ldap: { bindPassword: ldap.bindPassword } });
  t = await test('alice');
  check(t.ok === true, 'settings restored: the test passes again', t);
  // The 503s above counted as failed sign-ins for alice (GH#586); once their lock runs out she signs in.
  const back = await loginAfterLock(ALICE);
  check(back.status === 200, 'and alice signs in again', brief(back));
  alice = identity(back);

  // ---------------------------------------------------------------------------------------
  section('3. requireMfa for a directory account (GH#120)');
  await ok(admin, 'PATCH', `/api/v1/admin/profiles/${profile.id}`, { requireMfa: true });
  const gated = await login(ALICE);
  check(gated.status === 200, 'with requireMfa and no authenticator, alice still signs in', brief(gated));
  alice = identity(gated);
  const gatedMe = await ok(alice, 'GET', '/api/v1/auth/me');
  check(gatedMe.mfa.required === true && gatedMe.mfa.enrolmentRequired === true, '/auth/me: MFA required, enrolment required');
  const blocked = await call(alice, 'GET', '/api/v1/configuration-items?limit=1');
  check(blocked.status === 403 && code(blocked) === 'MFA_ENROLMENT_REQUIRED', 'everything else is 403 MFA_ENROLMENT_REQUIRED until she enrols', blocked.json);

  const badEnrol = await call(alice, 'POST', '/api/v1/auth/mfa/totp', { currentPassword: 'not-her-password' });
  check(badEnrol.status === 400 && field(badEnrol) === 'currentPassword', 'enrolment with a wrong directory password: 400 currentPassword', badEnrol.json);
  const enrol = await call(alice, 'POST', '/api/v1/auth/mfa/totp', { currentPassword: ALICE.password });
  check(enrol.status === 201 && typeof enrol.json?.secret === 'string', 'enrolment with the directory password: 201 with the secret', enrol.status);
  if (enrol.status !== 201) throw new Error('enrolment failed: the remaining checks need an authenticator');
  const nextCode = authenticator(enrol.json.secret);
  const confirm = await call(alice, 'POST', '/api/v1/auth/mfa/totp/confirm', { code: await nextCode() });
  check(confirm.status === 200 && confirm.json?.codes?.length === 10, 'a code confirms it: 200 with 10 recovery codes', brief(confirm));
  let recovery: string[] = confirm.json.codes;
  check((await call(alice, 'GET', '/api/v1/auth/me')).status === 401, 'the session from before the authenticator stops working (GH#510)');
  alice = renewed(confirm);
  const enrolledMe = await ok(alice, 'GET', '/api/v1/auth/me');
  check(enrolledMe.mfa.totpEnabled === true && enrolledMe.mfa.enrolmentRequired === false, '/auth/me: MFA on, no enrolment due');
  check((await call(alice, 'GET', '/api/v1/configuration-items?limit=1')).status === 200, 'the inventory is open again');

  const challenge = await login(ALICE);
  const mfaCookie = cookieOf(challenge, 'shadoucmdb_mfa');
  check(challenge.status === 401 && code(challenge) === 'MFA_REQUIRED' && mfaCookie, 'directory sign-in now answers 401 MFA_REQUIRED with the challenge cookie', challenge.json);
  check(!cookieOf(challenge, 'shadoucmdb_session'), 'and no session yet');
  const second = await call(null, 'POST', '/api/v1/auth/login/mfa', { code: await nextCode() }, mfaCookie);
  check(second.status === 200 && second.json?.user?.username === 'alice' && cookieOf(second, 'shadoucmdb_session'), 'a code completes the sign-in', brief(second));
  alice = identity(second);

  const reauth = async (who: Identity) => call(who, 'POST', '/api/v1/auth/mfa/recovery-codes', { currentPassword: ALICE.password, code: recovery[0] });
  let r = await reauth(alice);
  check(r.status === 200 && r.json?.codes?.length === 10, 'the directory password re-authenticates on the MFA routes (new recovery codes)', brief(r));
  if (r.status === 200) recovery = r.json.codes;

  const unavailable = async (what: string) => {
    const rc = await reauth(alice);
    check(rc.status === 503 && code(rc) === 'IDENTITY_PROVIDER_UNAVAILABLE', `${what}: MFA route (recovery codes) is 503 IDENTITY_PROVIDER_UNAVAILABLE`, brief(rc));
    const off = await call(alice, 'DELETE', '/api/v1/auth/mfa/totp', { currentPassword: ALICE.password, code: recovery[0] });
    check(off.status === 503 && code(off) === 'IDENTITY_PROVIDER_UNAVAILABLE', `${what}: MFA route (turn off) is 503 IDENTITY_PROVIDER_UNAVAILABLE`, brief(off));
    const l = await login(ALICE);
    check(l.status === 503 && code(l) === 'IDENTITY_PROVIDER_UNAVAILABLE', `${what}: sign-in is 503 IDENTITY_PROVIDER_UNAVAILABLE`, l.json);
    check(!LEAK.test(JSON.stringify([rc.json, off.json, l.json])), `${what}: no socket or TLS detail in the answers`);
  };
  await patch({ ldap: { url: CLOSED_URL, bindPassword: ldap.bindPassword } });
  await unavailable('unreachable URL');
  await patch({ ldap: { url: LDAP_URL, bindPassword: ldap.bindPassword } });

  directory('stop');
  await unavailable('directory stopped');
  directory('start');
  r = await reauth(alice);
  check(r.status === 200, 'directory back: re-authentication works again', brief(r));
  if (r.status === 200) recovery = r.json.codes;

  // Disabling through the API ends the account's sessions. A session that was not ended
  // (the provider disabled in the database, as reset-undecryptable or a race could leave it)
  // is refused on every request too, the MFA routes included (GH#250).
  await patch({ isEnabled: false });
  check((await call(alice, 'GET', '/api/v1/auth/me')).status === 401, 'disabling the directory ends her session');
  const disabledLogin = await login(ALICE);
  check(disabledLogin.status === 401 && code(disabledLogin) === 'UNAUTHENTICATED', 'and she cannot sign in through it (401)', disabledLogin.json);
  await patch({ isEnabled: true });
  const again = await login(ALICE);
  const viaRecovery = await call(null, 'POST', '/api/v1/auth/login/mfa', { code: recovery.shift() }, cookieOf(again, 'shadoucmdb_mfa'));
  check(again.status === 401 && viaRecovery.status === 200, 'enabled again: she signs in (password, then a recovery code)', brief(viaRecovery));
  alice = identity(viaRecovery);
  sql(`UPDATE cmdb.identity_providers SET is_enabled = false WHERE id = '${provider.id}'`);
  const leftover = await call(alice, 'GET', '/api/v1/auth/me');
  check(leftover.status === 401 && code(leftover) === 'UNAUTHENTICATED', 'directory disabled, session not ended: /auth/me is 401', brief(leftover));
  for (const [method, url, body] of [
    ['POST', '/api/v1/auth/mfa/totp', { currentPassword: ALICE.password }],
    ['POST', '/api/v1/auth/mfa/recovery-codes', { currentPassword: ALICE.password, code: recovery[0] }],
    ['DELETE', '/api/v1/auth/mfa/totp', { currentPassword: ALICE.password, code: recovery[0] }],
  ] as const) {
    const d = await call(alice, method, url, body);
    check(d.status === 401 && code(d) === 'UNAUTHENTICATED', `directory disabled, session not ended: ${method} ${url} is 401`, brief(d));
  }
  sql(`UPDATE cmdb.identity_providers SET is_enabled = true WHERE id = '${provider.id}'`);

  // ---------------------------------------------------------------------------------------
  section('4. Renamed entry: the name now finds another entry');
  r = await reauth(alice);
  check(r.status === 200, 'before the rename: her password re-authenticates', brief(r));
  if (r.status === 200) recovery = r.json.codes;
  // alice's entry keeps its entryUUID under a new name; a new entry takes the name "alice",
  // with the same password and group, so only its entryUUID differs.
  directory(
    'modify',
    `dn: uid=alice,ou=people,${SUFFIX}
changetype: modrdn
newrdn: uid=alice-former
deleteoldrdn: 1

dn: uid=alice,ou=people,${SUFFIX}
changetype: add
objectClass: inetOrgPerson
uid: alice
cn: Alice Newcomer
sn: Newcomer
displayName: Alice Newcomer
userPassword: ${ALICE.password}

dn: cn=cmdb-operators,ou=groups,${SUFFIX}
changetype: modify
add: member
member: uid=alice,ou=people,${SUFFIX}
`,
  );
  t = await test('alice');
  check(t.ok === true && t.user?.displayName === 'Alice Newcomer', 'the directory now answers "alice" with the new entry', t);
  r = await reauth(alice);
  check(r.status === 400 && field(r) === 'currentPassword', 'directory re-auth for the old account is refused: 400 currentPassword', brief(r));
  const off = await call(alice, 'DELETE', '/api/v1/auth/mfa/totp', { currentPassword: ALICE.password, code: recovery[0] });
  check(off.status === 400 && field(off) === 'currentPassword', 'MFA cannot be turned off with it either', brief(off));
  const stillOn = await ok(alice, 'GET', '/api/v1/auth/me');
  check(stillOn.mfa.totpEnabled === true, 'MFA is still on');
  const newcomer = await login(ALICE);
  const newcomerReason = await lastFailureReason(admin, 'alice');
  check(
    newcomer.status === 401 && sameAnswer(newcomer, wrong) && !cookieOf(newcomer, 'shadoucmdb_session'),
    'signing in as the new "alice" is refused, not linked to the old account: the same answer as a wrong password',
    { refused: newcomer.json, wrong: wrong.json },
  );
  check(newcomerReason === 'account_conflict', 'the audit log keeps the reason: account_conflict', newcomerReason);
}

try {
  await main();
} catch (e) {
  failures.push(`aborted: ${(e as Error).message}`);
  console.log(`\nABORTED: ${(e as Error).stack}`);
}
console.log(`\n${checks} checks, ${failures.length} failed`);
if (failures.length) {
  for (const f of failures) console.log(`  - ${f}`);
  process.exit(1);
}
