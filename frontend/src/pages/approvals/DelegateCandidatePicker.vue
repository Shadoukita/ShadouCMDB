<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref } from "vue";
import { findDelegateCandidates, type DelegateCandidate } from "../../api/approvals";
import { ApiError } from "../../api/client";
import type { Principal } from "../../api/services";
import PrincipalCombobox from "../../components/PrincipalCombobox.vue";
import { t } from "../../i18n";
import FormField from "../form/FormField.vue";

/**
 * Picks the delegate of your own approvals (GET /me/approval-delegations/candidates). Whether you may search the
 * user directory shows only in the first answer: until then, and with that right, it is a type-ahead combobox.
 * An answer with `exactMatchOnly` turns it into a username field: the username is looked up on blur or Enter, not
 * per keystroke (the API allows 30 such lookups a minute), and a match is confirmed by its display name. A miss
 * never says whether the account is missing, disabled or your own, because the API does not tell them apart.
 */
const props = defineProps<{ label: string; error?: string; picked?: string }>();
const emit = defineEmits<{ select: [candidate: DelegateCandidate | null] }>();

const MIN_CHARS = 2;
const MAX_CHARS = 100;
const exact = ref(false);
const username = ref("");
/** The username of the last finished lookup, and its match. */
const looked = ref<string | null>(null);
const found = ref<DelegateCandidate | null>(null);
const busy = ref(false);
const problem = ref<string | null>(null);
const input = ref<HTMLInputElement>();
let controller: AbortController | undefined;

const asPrincipal = (c: DelegateCandidate): Principal => ({ kind: "user", id: c.id, displayName: c.displayName, username: c.username, active: true });
const nameOf = (c: DelegateCandidate) => `${c.displayName} (${c.username})`;

async function search(q: string, signal: AbortSignal): Promise<Principal[]> {
  const res = await findDelegateCandidates(q, signal);
  if (res.exactMatchOnly && !signal.aborted) {
    exact.value = true;
    username.value = q;
    // Typed as a search, `q` may be half a username: a miss is not reported until the user looks it up.
    if (res.data[0]) settle(q, res.data[0]);
    void nextTick(() => input.value?.focus());
  }
  return res.data.map(asPrincipal);
}

function pickFromList(p: Principal) {
  emit("select", { id: p.id, displayName: p.displayName, username: p.username ?? "" });
}

function settle(q: string, match: DelegateCandidate | null) {
  looked.value = q;
  found.value = match;
  problem.value = null;
  emit("select", match);
}

function onEdit() {
  controller?.abort();
  busy.value = false;
  problem.value = null;
  if (looked.value !== null) {
    looked.value = null;
    found.value = null;
    emit("select", null);
  }
}

async function lookup() {
  const q = username.value.trim();
  if (q === "" || q === looked.value || busy.value) return;
  if (q.length < MIN_CHARS) {
    problem.value = t("services.owners.minChars");
    return;
  }
  const c = new AbortController();
  controller = c;
  busy.value = true;
  problem.value = null;
  try {
    const res = await findDelegateCandidates(q, c.signal);
    if (!c.signal.aborted) settle(q, res.data[0] ?? null);
  } catch (e) {
    if (c.signal.aborted) return;
    if (e instanceof ApiError && e.code === "RATE_LIMITED") problem.value = t("delegations.exact.rateLimited");
    else if (e instanceof ApiError && e.code === "VALIDATION_ERROR") problem.value = e.fieldErrors().q ?? e.message;
    else problem.value = e instanceof ApiError ? e.message : String(e);
  } finally {
    if (controller === c) busy.value = false;
  }
}

const notFound = computed(() => looked.value !== null && !found.value);
const exactError = computed(() => problem.value ?? (notFound.value ? t("delegations.exact.notFound") : props.error));
const exactHint = computed(() => {
  if (busy.value) return t("delegations.exact.searching");
  if (found.value) return t("delegations.exact.found", { name: nameOf(found.value) });
  return t("delegations.exact.hint");
});
/** Read out when a lookup finishes: the match, or that there is none. */
const announcement = computed(() => (busy.value ? "" : found.value ? exactHint.value : (exactError.value ?? "")));

onBeforeUnmount(() => controller?.abort());
</script>

<template>
  <PrincipalCombobox
    v-if="!exact"
    :label="label"
    kind="user"
    :search="search"
    :hint="error ?? (picked ? t('delegations.picked', { name: picked }) : t('delegations.delegateHint'))"
    @select="pickFromList"
  />
  <FormField v-else id="delegation-delegate-username" v-slot="f" :label="t('delegations.exact.label')" required :error="exactError" :hint="exactHint">
    <input
      :id="f.id"
      ref="input"
      v-model="username"
      type="text"
      class="mono"
      autocomplete="off"
      spellcheck="false"
      :maxlength="MAX_CHARS"
      :aria-invalid="f.invalid || undefined"
      :aria-describedby="f.describedBy"
      @input="onEdit"
      @blur="lookup"
      @keydown.enter.prevent="lookup"
    />
    <span class="sr-only" role="status" aria-live="polite">{{ announcement }}</span>
  </FormField>
</template>
