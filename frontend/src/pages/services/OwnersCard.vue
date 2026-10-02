<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { ApiError } from "../../api/client";
import { useReplaceOwners, type OwnerRole, type Principal, type PrincipalRef, type Service } from "../../api/services";
import ErrorAlert from "../../components/ErrorAlert.vue";
import PrincipalCombobox from "../../components/PrincipalCombobox.vue";
import { t } from "../../i18n";

/**
 * The Owners card of a business service (spec §5.3, §5.4): the technical and business owners by display
 * name, and for editors an inline form for both roles at once (PUT …/owners with the service's version).
 * Owners are ordered with Up and Down buttons, never by drag. A conflict keeps the unsaved selection and
 * shows the owners someone else saved next to it once reloaded; refused owners carry their error on the token.
 */
const props = defineProps<{ service: Service; canEdit: boolean; onReload: () => Promise<unknown> }>();
const emit = defineEmits<{ editing: [boolean] }>();
defineExpose({ edit });

const ROLES: OwnerRole[] = ["technical", "business"];
const roleTitle = (r: OwnerRole) => (r === "technical" ? t("services.owners.technical") : t("services.owners.business"));
/** The role as it reads inside a sentence ("Remove Ann as technical owner"). */
const roleWord = (r: OwnerRole) => (r === "technical" ? t("services.owners.role.technical") : t("services.owners.role.business"));
const kindText = (p: PrincipalRef) => (p.kind === "group" ? t("services.owners.group") : t("services.owners.user"));

const editing = ref(false);
const draft = ref<Record<OwnerRole, PrincipalRef[]>>({ technical: [], business: [] });
const save = useReplaceOwners(() => props.service.id);
/** The version the save was refused with: until the reload brings a newer one, Save would only fail again. */
const conflictVersion = ref<number | null>(null);
const reloading = ref(false);
const saved = ref("");
const pickers = ref<Partial<Record<OwnerRole, InstanceType<typeof PrincipalCombobox>>>>({});

const none = computed(() => props.service.owners.technical.length === 0 && props.service.owners.business.length === 0);
const max = computed(() => props.service.limits.maxOwnersPerRole);
const conflict = computed(() => conflictVersion.value !== null);
const reloaded = computed(() => conflict.value && props.service.version !== conflictVersion.value);
const apiError = computed(() => (save.error.value instanceof ApiError ? save.error.value : null));

/** 400 VALIDATION_ERROR details on `technical[3]` / `business[0]`, by role and token index. */
const tokenErrors = computed(() => {
  const out: Record<string, string> = {};
  if (apiError.value?.code !== "VALIDATION_ERROR") return out;
  for (const d of apiError.value.details) {
    const m = /^(technical|business)\[(\d+)\]$/.exec(d.field);
    if (!m) continue;
    const known = d.code === "not_found" || d.code === "duplicate";
    out[`${m[1]}:${m[2]}`] = known ? t(`services.owners.error.${d.code as "not_found" | "duplicate"}`) : d.message;
  }
  return out;
});
/** An error that no token carries (e.g. the role array as a whole, or a non-validation failure). */
const otherError = computed(() => {
  const e = save.error.value;
  if (!e || conflict.value) return null;
  if (apiError.value?.code === "VALIDATION_ERROR" && apiError.value.details.every((d) => /^(technical|business)\[\d+\]$/.test(d.field))) return null;
  return e;
});
const tokenErrorCount = computed(() => Object.keys(tokenErrors.value).length);
/** The tokens' errors were made for the selection as sent: a change to it makes them stale. */
watch(draft, () => tokenErrorCount.value > 0 && save.reset(), { deep: true });

function edit() {
  draft.value = { technical: [...props.service.owners.technical], business: [...props.service.owners.business] };
  conflictVersion.value = null;
  saved.value = "";
  save.reset();
  editing.value = true;
  emit("editing", true);
  void nextTick(() => pickers.value.technical?.focus());
}

function cancel() {
  editing.value = false;
  conflictVersion.value = null;
  save.reset();
  emit("editing", false);
}

function add(role: OwnerRole, p: Principal) {
  if (draft.value[role].some((o) => o.id === p.id)) return;
  draft.value[role] = [...draft.value[role], { kind: p.kind, id: p.id, displayName: p.displayName, active: p.active }];
}

function remove(role: OwnerRole, i: number) {
  const list = draft.value[role];
  draft.value[role] = list.filter((_, k) => k !== i);
  // Focus stays in the token list (the next token's remove button), else goes to the search field.
  void nextTick(() => {
    const next = document.getElementById(`owner-${role}-${Math.min(i, list.length - 2)}-remove`);
    if (next) next.focus();
    else pickers.value[role]?.focus();
  });
}

function moveOwner(role: OwnerRole, i: number, step: -1 | 1) {
  const list = [...draft.value[role]];
  const j = i + step;
  if (j < 0 || j >= list.length) return;
  [list[i], list[j]] = [list[j], list[i]];
  draft.value[role] = list;
  // Keep focus on the same button of the moved owner, so it can be pressed again; at the end of the list
  // that button is disabled, so the other one takes focus.
  void nextTick(() => {
    const same = document.getElementById(`owner-${role}-${j}-${step < 0 ? "up" : "down"}`) as HTMLButtonElement | null;
    (same && !same.disabled ? same : document.getElementById(`owner-${role}-${j}-${step < 0 ? "down" : "up"}`))?.focus();
  });
}

async function submit() {
  saved.value = "";
  const body = {
    version: props.service.version,
    technical: draft.value.technical.map(({ kind, id }) => ({ kind, id })),
    business: draft.value.business.map(({ kind, id }) => ({ kind, id })),
  };
  try {
    await save.mutateAsync(body);
    editing.value = false;
    conflictVersion.value = null;
    saved.value = t("services.owners.saved");
    emit("editing", false);
  } catch (e) {
    if (e instanceof ApiError && e.code === "VERSION_CONFLICT") conflictVersion.value = body.version;
    await nextTick();
    document.getElementById("owners-error")?.focus();
  }
}

async function reload() {
  reloading.value = true;
  try {
    await props.onReload();
  } finally {
    reloading.value = false;
  }
}
</script>

<template>
  <section class="panel owners-card" aria-labelledby="owners-title">
    <div class="panel-header">
      <h2 id="owners-title">{{ t("services.owners.title") }}</h2>
      <button v-if="canEdit && !editing" type="button" class="btn btn-sm" @click="edit">{{ t("services.owners.edit") }}</button>
    </div>
    <div class="panel-body">
      <p class="sr-only" role="status" aria-live="polite">{{ saved }}</p>
      <template v-if="!editing">
        <p v-if="none" class="note" role="note">{{ t("services.owners.none") }}</p>
        <div class="owner-roles">
          <div v-for="r in ROLES" :key="r" class="owner-role">
            <h3 :id="`owners-${r}`">{{ roleTitle(r) }}</h3>
            <ul v-if="service.owners[r].length" class="owner-list" :aria-labelledby="`owners-${r}`">
              <li v-for="o in service.owners[r]" :key="o.id">
                <bdi>{{ o.displayName }}</bdi> <span v-if="!o.active" class="badge warn">{{ t("services.owners.disabled") }}</span> <span class="muted">{{ kindText(o) }}</span>
              </li>
            </ul>
            <p v-else class="muted">{{ t("services.owners.noneInRole") }}</p>
          </div>
        </div>
      </template>

      <form v-else class="owners-form" novalidate @submit.prevent="submit">
        <div v-if="conflict" id="owners-error" class="alert alert-warn" role="alert" tabindex="-1">
          <p>{{ t("services.owners.conflict") }}</p>
          <button v-if="!reloaded" type="button" class="btn btn-sm" :disabled="reloading" @click="reload">
            {{ reloading ? t("common.loading") : t("services.owners.reload") }}
          </button>
          <template v-else>
            <p>{{ t("services.owners.reloaded") }}</p>
            <div class="owner-roles">
              <div v-for="r in ROLES" :key="r" class="owner-role">
                <h3>{{ t("services.owners.currentIn", { role: roleTitle(r) }) }}</h3>
                <ul v-if="service.owners[r].length" class="owner-list">
                  <li v-for="o in service.owners[r]" :key="o.id">
                    <bdi>{{ o.displayName }}</bdi> <span v-if="!o.active" class="badge warn">{{ t("services.owners.disabled") }}</span> <span class="muted">{{ kindText(o) }}</span>
                  </li>
                </ul>
                <p v-else class="muted">{{ t("services.owners.noneInRole") }}</p>
              </div>
            </div>
          </template>
        </div>
        <div v-else-if="tokenErrorCount > 0" id="owners-error" class="alert alert-error" role="alert" tabindex="-1">
          {{ t("services.owners.errorSummary", { n: tokenErrorCount }) }}
        </div>
        <div v-else-if="otherError" id="owners-error" tabindex="-1">
          <ErrorAlert :error="otherError" :title="t('services.owners.saveFailed')" />
        </div>

        <div class="owner-roles">
          <fieldset v-for="r in ROLES" :key="r" class="owner-role">
            <legend>{{ roleTitle(r) }}</legend>
            <ol v-if="draft[r].length" class="token-list">
              <li v-for="(o, i) in draft[r]" :key="o.id" :class="['token', { invalid: tokenErrors[`${r}:${i}`] }]">
                <span class="token-text">
                  <bdi>{{ o.displayName }}</bdi> <span v-if="!o.active" class="badge warn">{{ t("services.owners.disabled") }}</span> <span class="muted">{{ kindText(o) }}</span>
                </span>
                <span class="token-actions">
                  <button
                    :id="`owner-${r}-${i}-up`"
                    type="button"
                    class="btn btn-sm btn-icon"
                    :disabled="i === 0"
                    :aria-label="t('services.owners.moveUp', { name: o.displayName })"
                    :title="t('services.owners.moveUp', { name: o.displayName })"
                    @click="moveOwner(r, i, -1)"
                  >
                    <span aria-hidden="true">↑</span>
                  </button>
                  <button
                    :id="`owner-${r}-${i}-down`"
                    type="button"
                    class="btn btn-sm btn-icon"
                    :disabled="i === draft[r].length - 1"
                    :aria-label="t('services.owners.moveDown', { name: o.displayName })"
                    :title="t('services.owners.moveDown', { name: o.displayName })"
                    @click="moveOwner(r, i, 1)"
                  >
                    <span aria-hidden="true">↓</span>
                  </button>
                  <button
                    :id="`owner-${r}-${i}-remove`"
                    type="button"
                    class="btn btn-sm btn-icon"
                    :aria-label="t('services.owners.remove', { name: o.displayName, role: roleWord(r) })"
                    :title="t('services.owners.remove', { name: o.displayName, role: roleWord(r) })"
                    @click="remove(r, i)"
                  >
                    <span aria-hidden="true">×</span>
                  </button>
                </span>
                <span v-if="!o.active" class="token-note">{{ t("services.owners.disabledWarning", { name: o.displayName }) }}</span>
                <span v-if="tokenErrors[`${r}:${i}`]" class="token-error">{{ tokenErrors[`${r}:${i}`] }}</span>
              </li>
            </ol>
            <p v-else class="muted">{{ t("services.owners.noneInRole") }}</p>
            <PrincipalCombobox
              :ref="(c) => (pickers[r] = (c ?? undefined) as InstanceType<typeof PrincipalCombobox> | undefined)"
              :label="t('services.owners.addTo', { role: roleTitle(r) })"
              :exclude="draft[r].map((o) => o.id)"
              :disabled="draft[r].length >= max"
              :hint="draft[r].length >= max ? t('services.owners.limit', { max }) : undefined"
              @select="add(r, $event)"
            />
          </fieldset>
        </div>
        <p v-if="draft.technical.length === 0 && draft.business.length === 0" class="note" role="note">{{ t("services.owners.none") }}</p>
        <div class="actions owners-actions">
          <button type="submit" class="btn btn-primary" :disabled="save.isPending.value || (conflict && !reloaded)">
            {{ save.isPending.value ? t("common.saving") : t("services.owners.save") }}
          </button>
          <button type="button" class="btn" :disabled="save.isPending.value" @click="cancel">{{ t("common.cancel") }}</button>
        </div>
      </form>
    </div>
  </section>
</template>
