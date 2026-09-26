/** Keys of data model rows: lower case, digits and underscores, starting with a letter, at most 63 characters (the API's rule). */
export const KEY_PATTERN = /^[a-z][a-z0-9_]{0,62}$/;

/** Suggests a key from a display name ("Load balancer (L7)" → "load_balancer_l7"). */
export function suggestKey(name: string): string {
  const key = name
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^[^a-z]+/, "")
    .replace(/_+$/, "")
    .slice(0, 63)
    .replace(/_+$/, "");
  return key;
}

/** Client-side check before the round trip; the API validates again. */
export function keyError(key: string): string | undefined {
  if (!key) return "Required";
  if (!KEY_PATTERN.test(key)) return "Lower-case letters, digits and _, starting with a letter (max 63)";
  return undefined;
}
