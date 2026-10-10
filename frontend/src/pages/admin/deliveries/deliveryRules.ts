import type { ActionDelivery } from "../../../api/actionDeliveries";

/** The API retries a dead or held delivery of an e-mail or webhook action (inbox entries are written at fan-out). */
export const canRetry = (d: Pick<ActionDelivery, "status" | "kind">) => (d.status === "dead" || d.status === "held") && d.kind !== "inbox";

/** The API discards a pending, held or dead delivery; one being sent, delivered, skipped or discarded stays. */
export const canDiscard = (d: Pick<ActionDelivery, "status">) => d.status === "pending" || d.status === "held" || d.status === "dead";
