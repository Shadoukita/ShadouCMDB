// The Delete business service confirmation (spec SHAA-927 §5.8, GH#477): the dialog must say which
// services the deleted one is nested in before the operator can confirm, so the check gates the button.

/** "pending" while the check runs, "failed" when it answered an error, "succeeded" once it has data. */
export type ParentCheck = "pending" | "failed" | "succeeded";

/**
 * The state of the "included in N services" check. An error wins over earlier data: a failed refetch
 * must not let the dialog confirm on a count that may be stale.
 */
export function parentCheckState(q: { isError: boolean; isSuccess: boolean }): ParentCheck {
  if (q.isError) return "failed";
  return q.isSuccess ? "succeeded" : "pending";
}

/** Confirm stays disabled until the check succeeded; a pending or failed check never deletes silently. */
export function canConfirmDelete(check: ParentCheck): boolean {
  return check === "succeeded";
}

/** The services that include this one directly: it is removed from them. */
export function directParentCount(services: readonly { direct: boolean }[] | undefined): number {
  return (services ?? []).filter((s) => s.direct).length;
}
