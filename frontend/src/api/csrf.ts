// Which CSRF cookie to echo. Over HTTPS the API sets `__Host-shadoucmdb_csrf`
// (a cookie no other subdomain can plant); over plain HTTP, `shadoucmdb_csrf`.
// The API reads the session from the `__Host-` cookie whenever the browser
// sends one, so the CSRF token must come from its `__Host-` partner too: a
// plain-named cookie next to it may be stale or planted and is never used.
// Kept free of browser globals so the e2e suite can test it directly.

const HOST_CSRF_COOKIE = "__Host-shadoucmdb_csrf";
const CSRF_COOKIE = "shadoucmdb_csrf";

/** The CSRF token in a `document.cookie` string, or undefined when there is none to trust. */
export function csrfFromCookies(cookies: string): string | undefined {
  const values = new Map<string, string>();
  for (const pair of cookies.split(";")) {
    const at = pair.indexOf("=");
    if (at < 0) continue;
    const name = pair.slice(0, at).trim();
    // The first of a repeated name, as the API reads it.
    if (!values.has(name)) values.set(name, pair.slice(at + 1).trim());
  }
  const value = values.has(HOST_CSRF_COOKIE) ? values.get(HOST_CSRF_COOKIE) : values.get(CSRF_COOKIE);
  return value || undefined;
}
