/**
 * Cloning a CI (gap G11): a new CI of the same class whose form starts from the source's values. The copy is made
 * in the browser and saved through the normal create path (POST /configuration-items), so the create right,
 * validation and the audit trail are those of any new CI.
 *
 * Copied: the class attributes and the criticality. Not copied: the ident (generated, or chosen by an
 * administrator), the validity period (the new CI is valid from now), relationships, history and workflow state.
 * Left empty for the operator to fill in, because two CIs should not share them: the class's title attribute (the
 * CI's name), the Person's Name and Email (`systemRole`; the Email is unique), IP addresses (one device's address)
 * and references to CIs the user may not view. Fields an active workflow drives on the source start at their
 * default, since a new CI takes the workflow's initial value (the API refuses another one). Attribute definitions
 * carry no "unique" flag yet, so other identifying text fields (a serial number) are copied and shown for review.
 */

interface CloneAttr {
  id: string;
  key: string;
  dataType: string;
  systemRole?: string | null;
}

/**
 * The attribute keys the clone leaves empty (see above), in the class's order. A reference to a CI the user may not
 * view (`hidden` in the source's `attributeReferences`) is left empty too: it would be copied unseen.
 */
export function cloneClearedKeys(
  defs: readonly CloneAttr[],
  titleAttributeId: string | null | undefined,
  references?: Readonly<Record<string, { hidden?: boolean }>>,
): string[] {
  return defs.filter((d) => d.id === titleAttributeId || !!d.systemRole || d.dataType === "ip" || !!references?.[d.key]?.hidden).map((d) => d.key);
}

export interface CloneSource {
  /** The source CI's stored attribute values. */
  attributes: Readonly<Record<string, unknown>>;
  /** Left empty on the new CI. */
  cleared: ReadonlySet<string>;
  /** Start at the attribute's default (fields a workflow drives on the source). */
  reset: ReadonlySet<string>;
}

/** The value a clone's attribute starts from: the source's, else the attribute's default. */
export function clonedValue(src: CloneSource, d: { key: string; defaultValue: unknown }): unknown {
  if (src.cleared.has(d.key)) return undefined;
  if (src.reset.has(d.key)) return d.defaultValue;
  return src.attributes[d.key] ?? undefined;
}
