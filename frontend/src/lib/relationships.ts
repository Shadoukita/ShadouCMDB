import type { Relationship } from "../api/queries";

/** An edge seen from one CI: "runs on X" when it is the source, "hosts X" when it is the target. */
export function describeEdge(r: Relationship, ciId: string) {
  const outgoing = r.sourceCiId === ciId;
  return {
    outgoing,
    label: outgoing || !r.type.isDirectional ? r.type.forwardLabel : r.type.reverseLabel,
    other: outgoing ? r.target : r.source,
  };
}
