import type { WorkflowWarning } from "../../../api/workflows";

/** The `UNINSTANCED_CIS` warning in one sentence, with what to do about it. */
export function uninstancedText(w: Pick<WorkflowWarning, "count">): string {
  const n = w.count;
  const what =
    n === null
      ? "Some CIs this workflow covers have no running instance (the number spans types you may not view)."
      : n === 0
        ? "Every live CI this workflow covers already has a running instance."
        : `${n.toLocaleString()} live ${n === 1 ? "CI this workflow covers has" : "CIs this workflow covers have"} no running instance.`;
  return n === 0 ? what : `${what} Their state field is locked until an instance is started on them: adopt them with the workflow's bootstrap.`;
}
