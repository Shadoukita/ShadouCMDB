<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { ApiError } from "../api/client";
import { changedFields } from "../lib/changes";
import { keyError, suggestKey } from "../lib/keys";
import FormErrorBanner from "../pages/form/FormErrorBanner.vue";
import FormField from "../pages/form/FormField.vue";
import FormDialog from "./FormDialog.vue";

/**
 * Create/edit dialog for simple rows (relationship types, lookup lists and
 * their values), driven by a field list. The caller's `save` sends the body;
 * API field errors show next to their fields.
 */
export interface FieldSpec {
  name: string;
  label: string;
  type: "text" | "key" | "textarea" | "checkbox" | "select" | "color";
  required?: boolean;
  /** Shown but read-only when editing (keys and settings fixed after creation). */
  createOnly?: boolean;
  /** Choices of a select; a function when they depend on the other fields (the first one is taken when the chosen one goes). */
  options?: SelectOption[] | ((values: Record<string, Value>, record: Record<string, unknown> | null) => SelectOption[]);
  /** For "key": suggested from this field while the key has not been typed in. */
  from?: string;
  hint?: string;
  wide?: boolean;
  /** Label next to a checkbox. */
  text?: string;
}
type Value = string | boolean;
export interface SelectOption {
  value: string;
  label: string;
  depth?: number;
}

const props = defineProps<{
  open: boolean;
  title: string;
  submitLabel: string;
  fields: FieldSpec[];
  /** The row being edited, or null to create. */
  record: Record<string, unknown> | null;
  /** Initial values for a new row. */
  defaults?: Record<string, Value>;
  /** Sends the body (create: every field; edit: only the fields the user changed). Resolves with a confirmation. */
  save: (body: Record<string, unknown>, isNew: boolean) => Promise<string>;
  idPrefix: string;
}>();
const emit = defineEmits<{ close: []; saved: [message: string] }>();

const values = ref<Record<string, Value>>({});
const keyTouched = ref(false);
const busy = ref(false);
const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});
/** The body as seeded, to send only changed fields on edit. */
let initial: Record<string, unknown> = {};
const isNew = computed(() => !props.record);

function seed() {
  const v: Record<string, Value> = {};
  for (const f of props.fields) {
    const raw = props.record ? props.record[f.name] : props.defaults?.[f.name];
    v[f.name] = f.type === "checkbox" ? raw === true || (raw === undefined && !props.record && f.name === "isActive") : raw == null ? "" : String(raw);
  }
  values.value = v;
  initial = body();
  keyTouched.value = !!props.record;
  error.value = null;
  local.value = {};
}
watch(
  () => [props.open, props.record] as const,
  ([open]) => open && seed(),
  { immediate: true },
);

// Keys follow the name until typed in.
watch(
  () => props.fields.filter((f) => f.type === "key" && f.from).map((f) => values.value[f.from!]),
  () => {
    if (!isNew.value || keyTouched.value) return;
    for (const f of props.fields) if (f.type === "key" && f.from) values.value[f.name] = suggestKey(String(values.value[f.from] ?? ""));
  },
);

function optionsOf(f: FieldSpec): SelectOption[] {
  return typeof f.options === "function" ? f.options(values.value, props.record) : (f.options ?? []);
}
// Options that follow other fields: a choice they no longer offer falls back to the first one.
watch(
  () => props.fields.filter((f) => typeof f.options === "function").map((f) => optionsOf(f).map((o) => o.value).join("|")),
  () => {
    for (const f of props.fields) {
      if (typeof f.options !== "function") continue;
      const opts = optionsOf(f);
      if (opts.length > 0 && !opts.some((o) => o.value === values.value[f.name])) values.value[f.name] = opts[0].value;
    }
  },
);

// A declaration, not an arrow: seed() runs (and calls body()) before this line on first setup.
function readOnly(f: FieldSpec) {
  return !isNew.value && !!f.createOnly;
}
const fieldErrors = computed(() => ({ ...(error.value instanceof ApiError ? error.value.fieldErrors() : {}), ...local.value }));
const unplaced = computed(() =>
  error.value instanceof ApiError ? error.value.details.filter((d) => !props.fields.some((f) => f.name === d.field)) : [],
);

function body(): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const f of props.fields) {
    if (readOnly(f)) continue;
    const v = values.value[f.name];
    if (f.type === "checkbox") out[f.name] = v === true;
    else if (f.type === "key") out[f.name] = String(v).trim();
    else {
      const s = String(v ?? "").trim();
      // Optional fields are cleared with null; required ones are sent as typed so the API can name them.
      out[f.name] = s === "" && !f.required ? null : f.type === "textarea" ? String(v) : s;
    }
  }
  return out;
}

async function submit() {
  error.value = null;
  const errs: Record<string, string> = {};
  for (const f of props.fields) {
    if (readOnly(f)) continue;
    const v = values.value[f.name];
    if (f.type === "key") {
      const e = keyError(String(v));
      if (e) errs[f.name] = e;
    } else if (f.required && f.type !== "checkbox" && String(v ?? "").trim() === "") errs[f.name] = "Required";
  }
  local.value = errs;
  const first = Object.keys(errs)[0];
  if (first) {
    document.getElementById(`${props.idPrefix}-${first}`)?.focus();
    return;
  }
  const out = isNew.value ? body() : changedFields(body(), initial);
  if (!isNew.value && Object.keys(out).length === 0) {
    emit("close");
    return;
  }
  busy.value = true;
  try {
    const message = await props.save(out, isNew.value);
    emit("saved", message);
    emit("close");
  } catch (e) {
    error.value = e;
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <FormDialog :open="open" :title="title" :submit-label="submitLabel" :busy="busy" @submit="submit" @cancel="emit('close')">
    <FormErrorBanner v-if="error" :error="error" :unplaced="unplaced" />
    <div class="form-grid">
      <template v-for="(f, i) in fields" :key="f.name">
        <div v-if="f.type === 'checkbox'" class="field">
          <span class="label">{{ f.label }}</span>
          <label class="checkbox-row">
            <input :id="`${idPrefix}-${f.name}`" v-model="values[f.name]" type="checkbox" :disabled="readOnly(f)" />
            {{ f.text ?? f.label }}
          </label>
          <span v-if="fieldErrors[f.name]" class="error">{{ fieldErrors[f.name] }}</span>
          <span v-if="f.hint" class="hint">{{ f.hint }}</span>
        </div>
        <FormField
          v-else
          :id="`${idPrefix}-${f.name}`"
          v-slot="p"
          :label="f.label"
          :required="f.required || (f.type === 'key' && !readOnly(f))"
          :error="fieldErrors[f.name]"
          :hint="readOnly(f) ? 'Fixed after creation' : f.hint"
          :wide="f.wide || f.type === 'textarea'"
        >
          <textarea
            v-if="f.type === 'textarea'"
            :id="p.id"
            v-model="values[f.name] as string"
            rows="2"
            :aria-invalid="p.invalid || undefined"
            :aria-describedby="p.describedBy"
          />
          <select
            v-else-if="f.type === 'select'"
            :id="p.id"
            v-model="values[f.name] as string"
            :disabled="readOnly(f)"
            :aria-invalid="p.invalid || undefined"
            :aria-describedby="p.describedBy"
          >
            <option v-for="o in optionsOf(f)" :key="o.value" :value="o.value">{{ "  ".repeat(o.depth ?? 0) }}{{ o.label }}</option>
          </select>
          <div v-else-if="f.type === 'color'" class="inline-control">
            <input
              :id="p.id"
              type="color"
              :value="(values[f.name] as string) || '#1f5fbf'"
              :aria-describedby="p.describedBy"
              @input="values[f.name] = ($event.target as HTMLInputElement).value"
            />
            <span class="mono">{{ values[f.name] || "none" }}</span>
            <button v-if="values[f.name]" type="button" class="btn btn-sm" @click="values[f.name] = ''">No colour</button>
          </div>
          <input
            v-else
            :id="p.id"
            v-model="values[f.name] as string"
            type="text"
            :class="{ mono: f.type === 'key' }"
            :spellcheck="f.type !== 'key'"
            :readonly="readOnly(f)"
            :autofocus="i === 0"
            :aria-invalid="p.invalid || undefined"
            :aria-describedby="p.describedBy"
            @input="f.type === 'key' && (keyTouched = true)"
          />
        </FormField>
      </template>
    </div>
  </FormDialog>
</template>
