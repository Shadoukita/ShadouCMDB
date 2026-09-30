<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { ApiError } from "../../../api/client";
import { useCreateArea, usePatch, type Area, type AreaCreateBody, type AreaUpdateBody } from "../../../api/datamodel";
import ClassBadge from "../../../components/ClassBadge.vue";
import FormDialog from "../../../components/FormDialog.vue";
import SchemaChangeDialog from "../../../components/SchemaChangeDialog.vue";
import TechnicalNameField from "../../../components/TechnicalNameField.vue";
import { changedFields } from "../../../lib/changes";
import { CLASS_ICONS, classIcon } from "../../../lib/classIcons";
import { keyError } from "../../../lib/keys";
import { useSchemaChangeFlow } from "../../../lib/schemaChange";
import FormErrorBanner from "../../form/FormErrorBanner.vue";
import FormField from "../../form/FormField.vue";

/**
 * Create or edit an area: a menu tab and the PostgreSQL schema its types' tables
 * live in. The technical name (the schema) is chosen on create and fixed after;
 * renaming changes only the tab's label. Creating previews the DDL first.
 */
const props = defineProps<{ open: boolean; area: Area | null; nextSortOrder: number }>();
const emit = defineEmits<{ close: []; saved: [message: string] }>();

const create = useCreateArea();
const update = usePatch<Area>("areas");
const flow = useSchemaChangeFlow();
const busy = computed(() => create.isPending.value || update.isPending.value || flow.state.loading);
const isNew = computed(() => !props.area);

const name = ref("");
const key = ref("");
const description = ref("");
const icon = ref("");
const color = ref("");
const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});
/** The form as opened, to send only changed fields on edit. */
let initial: AreaUpdateBody = {};

watch(
  () => [props.open, props.area] as const,
  ([open]) => {
    if (!open) return;
    const a = props.area;
    name.value = a?.name ?? "";
    key.value = a?.key ?? "";
    description.value = a?.description ?? "";
    icon.value = a?.icon ?? "";
    color.value = a?.color ?? "";
    error.value = null;
    local.value = {};
    initial = formBody();
  },
  { immediate: true },
);

function formBody(): AreaUpdateBody {
  return {
    name: name.value.trim(),
    description: description.value.trim() || null,
    icon: icon.value || null,
    color: color.value || null,
  };
}

const unknownIcon = computed(() => !!icon.value && !classIcon(icon.value));
const fieldErrors = computed(() => ({ ...(error.value instanceof ApiError ? error.value.fieldErrors() : {}), ...local.value }));
const FIELDS = ["name", "key", "description", "icon", "color"];
const unplaced = computed(() => (error.value instanceof ApiError ? error.value.details.filter((d) => !FIELDS.includes(d.field)) : []));

async function submit() {
  error.value = null;
  const errs: Record<string, string> = {};
  if (!name.value.trim()) errs.name = "Required";
  if (isNew.value) {
    const k = keyError(key.value);
    if (k) errs.key = k;
  }
  local.value = errs;
  if (Object.keys(errs).length) return;
  const common = formBody();
  if (isNew.value) {
    const body: AreaCreateBody = { ...common, name: common.name!, key: key.value, sortOrder: props.nextSortOrder };
    const outcome = await flow.run({
      title: `Create area “${body.name}”`,
      intro: `The area becomes a menu tab and the PostgreSQL schema “${body.key}”; the tables of its types are created in it.`,
      preview: { operation: "createArea", body },
      apply: () => create.mutateAsync(body),
      applyLabel: "Create area",
      alwaysShow: true,
    });
    if (outcome.status === "applied") {
      emit("saved", `Created area ${body.name} (schema ${body.key}).`);
      emit("close");
    } else if (outcome.status === "refused") error.value = outcome.error;
    return;
  }
  const a = props.area!;
  const changed = changedFields(common, initial);
  if (Object.keys(changed).length === 0) {
    emit("close");
    return;
  }
  const outcome = await flow.run({
    title: `Save area “${common.name}”`,
    preview: { operation: "updateArea", id: a.id, body: changed },
    apply: () => update.mutateAsync({ id: a.id, body: changed }),
    applyLabel: "Save area",
  });
  if (outcome.status === "applied") {
    emit("saved", `Saved area ${common.name}.`);
    emit("close");
  } else if (outcome.status === "refused") error.value = outcome.error;
}
</script>

<template>
  <FormDialog
    :open="open"
    :title="isNew ? 'New area' : `Edit area “${area?.name}”`"
    :submit-label="isNew ? 'Preview and create…' : 'Save area'"
    :busy="busy"
    @submit="submit"
    @cancel="emit('close')"
  >
    <FormErrorBanner v-if="error" :error="error" :unplaced="unplaced" />
    <div class="form-grid">
      <FormField id="area-name" v-slot="p" label="Name" required :error="fieldErrors.name" hint="The menu tab's label, e.g. Bestand">
        <input :id="p.id" v-model="name" type="text" maxlength="200" autofocus :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
      </FormField>
      <TechnicalNameField id="area-key" v-model="key" kind="area" :name="name" :editable="isNew" :location="area?.key" :error="fieldErrors.key" />
      <FormField id="area-icon" v-slot="p" label="Icon" :error="fieldErrors.icon">
        <div class="inline-control">
          <select :id="p.id" v-model="icon" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
            <option value="">— none —</option>
            <option v-for="i in CLASS_ICONS" :key="i.key" :value="i.key">{{ i.label }}</option>
            <option v-if="unknownIcon" :value="icon">{{ icon }} (custom)</option>
          </select>
          <ClassBadge :icon="icon" :color="color || '#56606d'" />
        </div>
      </FormField>
      <FormField id="area-color" v-slot="p" label="Colour" :error="fieldErrors.color" hint="Shown with the icon on the menu tab">
        <div class="inline-control">
          <input :id="p.id" type="color" :value="color || '#1f5fbf'" :aria-describedby="p.describedBy" @input="color = ($event.target as HTMLInputElement).value" />
          <span class="mono">{{ color || "none" }}</span>
          <button v-if="color" type="button" class="btn btn-sm" @click="color = ''">No colour</button>
        </div>
      </FormField>
      <FormField id="area-description" v-slot="p" label="Description" wide :error="fieldErrors.description">
        <textarea :id="p.id" v-model="description" rows="2" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
      </FormField>
    </div>
  </FormDialog>
  <SchemaChangeDialog :flow="flow" />
</template>
