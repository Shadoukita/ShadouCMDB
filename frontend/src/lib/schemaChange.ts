import { reactive } from "vue";
import { ApiError } from "../api/client";
import { previewSchemaChange, type PreviewRequest, type SchemaChangePreview } from "../api/schemaChanges";

/**
 * The "preview, then apply" flow every data model change goes through. `run`
 * asks the API to dry-run the operation; the SchemaChangeDialog bound to the
 * flow shows the DDL and its effect on stored data, and applies the change only
 * when the administrator confirms. A change that runs no DDL and touches no data
 * (a rename, a new icon) is applied straight away unless `alwaysShow` is set.
 *
 * Errors a form can place next to its fields (validation, an unusable technical
 * name) are returned without opening the dialog; a guard's refusal (a type
 * change that would not convert every value, a field made required while CIs
 * lack a value, an object still in use) is shown in the dialog.
 */
export interface SchemaChangeOptions {
  /** "Create area “Bestand”" */
  title: string;
  preview: PreviewRequest;
  /** Performs the change through its real endpoint. Gets the typed confirmation for purges. */
  apply: (confirm: string) => Promise<unknown>;
  applyLabel: string;
  /** One sentence above the preview. */
  intro?: string;
  /** Destructive: red apply button. */
  danger?: boolean;
  /** Purge: the technical name the administrator must type before applying. */
  confirmName?: string;
  /** Show the dialog even when the change runs no DDL. */
  alwaysShow?: boolean;
}

export type SchemaChangeOutcome =
  | { status: "applied"; result: unknown }
  | { status: "cancelled" }
  /** The preview or the change was refused; the form should show `error`. */
  | { status: "refused"; error: unknown };

/** Codes whose details name form fields; they are shown by the form, not the dialog. */
const FORM_CODES = new Set(["VALIDATION_ERROR", "INVALID_NAME"]);

export function useSchemaChangeFlow() {
  const state = reactive({
    open: false,
    options: null as SchemaChangeOptions | null,
    loading: false,
    preview: null as SchemaChangePreview | null,
    /** Why the preview was refused (shown instead of the DDL). */
    refusal: null as unknown,
    applying: false,
    applyError: null as unknown,
    typed: "",
  });
  let settle: ((o: SchemaChangeOutcome) => void) | null = null;

  function finish(outcome: SchemaChangeOutcome) {
    state.open = false;
    state.applying = false;
    settle?.(outcome);
    settle = null;
  }

  async function run(options: SchemaChangeOptions): Promise<SchemaChangeOutcome> {
    if (settle) finish({ status: "cancelled" });
    Object.assign(state, { options, loading: true, preview: null, refusal: null, applying: false, applyError: null, typed: "" });
    const outcome = new Promise<SchemaChangeOutcome>((resolve) => (settle = resolve));
    // The dialog opens at once for purges and forced previews (the user asked for it); otherwise only when there is something to show.
    if (options.confirmName || options.alwaysShow) state.open = true;
    try {
      const preview = await previewSchemaChange(options.preview);
      state.preview = preview;
      state.loading = false;
      const empty = preview.statements.length === 0 && preview.impact.length === 0;
      if (empty && !state.open) {
        await apply();
        return outcome;
      }
      state.open = true;
    } catch (e) {
      state.loading = false;
      if (e instanceof ApiError && FORM_CODES.has(e.code) && !state.open) {
        finish({ status: "refused", error: e });
        return outcome;
      }
      state.refusal = e;
      state.open = true;
    }
    return outcome;
  }

  async function apply() {
    const o = state.options;
    if (!o || state.applying) return;
    if (o.confirmName && state.typed.trim() !== o.confirmName) return;
    state.applying = true;
    state.applyError = null;
    try {
      const result = await o.apply(state.typed.trim());
      finish({ status: "applied", result });
    } catch (e) {
      state.applying = false;
      if (!state.open && e instanceof ApiError && FORM_CODES.has(e.code)) {
        finish({ status: "refused", error: e });
        return;
      }
      state.applyError = e;
      state.open = true;
    }
  }

  function cancel() {
    if (state.applying) return;
    finish(state.refusal ? { status: "refused", error: state.refusal } : { status: "cancelled" });
  }

  return { state, run, apply, cancel };
}

export type SchemaChangeFlow = ReturnType<typeof useSchemaChangeFlow>;

/** Impact kinds that lose or rewrite data, highlighted in the preview. */
export const DESTRUCTIVE_IMPACT = new Set(["drop_column", "drop_table", "drop_schema", "rewrite", "not_null", "data_deleted", "warning"]);
