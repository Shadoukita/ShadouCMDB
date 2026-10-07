// When an identity provider's server address changes, its stored secret must be entered again:
// otherwise an administrator could point the provider at a host they control and receive the stored
// bind password / client secret in clear (GH#238). The API refuses such a PATCH with 422
// `secret_required`; this module lets the edit form ask for the secret before that round trip.
// No imports, so `node --test` runs it without a bundler.

export type SecretField = "oidc.clientSecret" | "ldap.bindPassword";

/** The stored provider, as far as this rule needs it. */
export interface StoredAddress {
  oidc?: { issuerUrl: string; clientSecretSet: boolean } | null;
  ldap?: { url: string; bindDn?: string | null; bindPasswordSet: boolean } | null;
}

/** The address fields as typed in the form. */
export interface TypedAddress {
  kind: "oidc" | "ldap";
  issuerUrl: string;
  url: string;
  bindDn: string;
}

export const SECRET_REQUIRED = "secret_required";

const DEFAULT_PORTS: Record<string, string> = { "ldap:": "389", "ldaps:": "636" };

/** What the API compares for an LDAP URL: scheme, host and port ("ldaps://DC1.example.com" = "ldaps://dc1.example.com:636"). */
export function ldapEndpoint(url: string): string {
  const u = url.trim();
  try {
    const parsed = new URL(u);
    return `${parsed.protocol}//${parsed.hostname.toLowerCase()}:${parsed.port || DEFAULT_PORTS[parsed.protocol] || ""}`;
  } catch {
    return u;
  }
}

/** What the API compares for an issuer URL: scheme, host, port and path, a trailing slash aside. */
export function issuerKey(url: string): string {
  const u = url.trim();
  try {
    const parsed = new URL(u);
    return `${parsed.protocol}//${parsed.host}${parsed.pathname.replace(/\/+$/, "")}`;
  } catch {
    return u;
  }
}

/**
 * The secret field that must be entered again, or null: the provider has a stored secret and the
 * form changes the address it would be sent to (OIDC issuer URL; LDAP URL or bind DN).
 * Typing the stored value back removes the requirement.
 */
export function secretReentryField(stored: StoredAddress | undefined, form: TypedAddress): SecretField | null {
  if (!stored) return null;
  if (form.kind === "oidc") {
    const o = stored.oidc;
    return o?.clientSecretSet && issuerKey(form.issuerUrl) !== issuerKey(o.issuerUrl) ? "oidc.clientSecret" : null;
  }
  const l = stored.ldap;
  if (!l?.bindPasswordSet) return null;
  // Without a bind DN the search is anonymous and the stored password is removed: nothing to enter.
  if (!form.bindDn.trim()) return null;
  const moved = ldapEndpoint(form.url) !== ldapEndpoint(l.url) || form.bindDn.trim() !== (l.bindDn ?? "").trim();
  return moved ? "ldap.bindPassword" : null;
}

/**
 * The secret's form value once the requirement is known. Required: "keep stored" (undefined) turns
 * into an empty input to fill in. No longer required: an input left empty goes back to "keep stored".
 * A typed value, or a removal (null), is left as it is.
 */
export function syncSecret(value: string | null | undefined, required: boolean): string | null | undefined {
  if (required && value === undefined) return "";
  if (!required && value === "") return undefined;
  return value;
}

/** Whether a required secret is still missing: nothing typed (a removal counts as an answer). */
export const secretMissing = (value: string | null | undefined) => value === undefined || value === "";

/** The fields a 422 names with code `secret_required` (e.g. the stored address changed in the meantime). */
export function secretRequiredFields(error: unknown): SecretField[] {
  const details = (error as { details?: unknown } | null)?.details;
  if (!Array.isArray(details)) return [];
  return details
    .filter((d): d is { field: SecretField } => d?.code === SECRET_REQUIRED && (d.field === "oidc.clientSecret" || d.field === "ldap.bindPassword"))
    .map((d) => d.field);
}
