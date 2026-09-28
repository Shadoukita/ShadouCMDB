<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { useAllProfiles, useCreateApiToken, useUserList, type ApiToken } from "../../api/admin";
import { ApiError } from "../../api/client";
import { MAX_PAGE } from "../../api/queries";
import { formatDateTime } from "../../lib/format";
import { useSessionStore } from "../../stores/session";
import FormErrorBanner from "../form/FormErrorBanner.vue";
import FormField from "../form/FormField.vue";

/**
 * New API token: name, owner, profile and expiry, then the secret, shown once.
 * The secret lives only in this component's state while the dialog is open: it
 * is not cached, not put in the URL, and cleared when the dialog closes.
 */
const props = defineProps<{ open: boolean }>();
const emit = defineEmits<{ close: [] }>();
const session = useSessionStore();
const create = useCreateApiToken();
const profiles = useAllProfiles();
const users = useUserList(() => ({ isActive: "true", sort: "username", limit: MAX_PAGE }), () => props.open);

/** The API accepts at most 366 days; a custom date is capped a day earlier so the end of that day still fits. */
const MAX_DAYS = 365;
const PRESETS = [
  { days: 7, label: "7 days" },
  { days: 30, label: "30 days" },
  { days: 90, label: "90 days" },
  { days: 180, label: "180 days" },
  { days: 365, label: "1 year" },
];

const dialog = ref<HTMLDialogElement>();
const nameInput = ref<HTMLInputElement>();
const secretInput = ref<HTMLInputElement>();
const name = ref("");
const userId = ref("");
const profileId = ref("");
const expiry = ref("90");
const customDate = ref("");
const local = ref<Record<string, string>>({});
/** The created token and its secret: the only copy the UI ever holds. */
const created = ref<{ token: ApiToken; secret: string } | null>(null);
const copyState = ref<"" | "copied" | "failed">("");

watch(
  () => props.open,
  async (open) => {
    const d = dialog.value;
    if (!d) return;
    if (open) {
      reset();
      if (!d.open) d.showModal();
      await nextTick();
      nameInput.value?.focus();
    } else if (d.open) d.close();
  },
  { flush: "post" },
);

function reset() {
  create.reset();
  created.value = null;
  copyState.value = "";
  name.value = "";
  userId.value = "";
  profileId.value = "";
  expiry.value = "90";
  customDate.value = "";
  local.value = {};
}

function close(e?: Event) {
  e?.preventDefault();
  if (create.isPending.value) return;
  reset();
  emit("close");
}

const isoDate = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
const addDays = (days: number) => new Date(Date.now() + days * 86_400_000);
const minDate = computed(() => isoDate(addDays(1)));
const maxDate = computed(() => isoDate(addDays(MAX_DAYS)));

/** The expiry to send: a preset counts from now; a custom date means the end of that day, local time. */
const expiresAt = computed<Date | null>(() => {
  if (expiry.value !== "custom") return addDays(Number(expiry.value));
  if (!customDate.value) return null;
  const d = new Date(`${customDate.value}T23:59:59`);
  return Number.isNaN(d.getTime()) ? null : d;
});

const ownerOptions = computed(() => (users.data.value?.data ?? []).filter((u) => u.id !== session.user?.id));
const moreUsers = computed(() => (users.data.value?.page.total ?? 0) > (users.data.value?.data.length ?? 0));

const apiError = computed(() => (create.error.value instanceof ApiError ? create.error.value : null));
const apiErrors = computed(() => apiError.value?.fieldErrors() ?? {});
const errorFor = (field: string) => local.value[field] ?? apiErrors.value[field];
const PLACED = ["name", "userId", "profileId", "expiresAt"];
const unplaced = computed(() => apiError.value?.details.filter((d) => !PLACED.includes(d.field)) ?? []);
/** 403: the chosen owner holds permissions the caller does not, so the caller may not mint a token acting as them. */
const forbidden = computed(() => apiError.value?.code === "FORBIDDEN");

function submit() {
  const errs: Record<string, string> = {};
  if (!name.value.trim()) errs.name = "Name the token after what uses it, e.g. “backup script”.";
  if (!profileId.value) errs.profileId = "Choose the profile that limits what the token may do.";
  if (!expiresAt.value) errs.expiresAt = "Choose the day the token expires.";
  local.value = errs;
  if (Object.keys(errs).length > 0 || !expiresAt.value) return;
  create.mutate(
    {
      name: name.value.trim(),
      profileId: profileId.value,
      expiresAt: expiresAt.value.toISOString(),
      ...(userId.value ? { userId: userId.value } : {}),
    },
    {
      onSuccess: async (result) => {
        created.value = { token: result.token, secret: result.secret };
        // Drop the mutation's copy of the answer, so the secret is held in one place only.
        create.reset();
        await nextTick();
        secretInput.value?.focus();
        secretInput.value?.select();
      },
    },
  );
}

async function copySecret() {
  const secret = created.value?.secret;
  if (!secret) return;
  try {
    await navigator.clipboard.writeText(secret);
    copyState.value = "copied";
  } catch {
    // No Clipboard API (plain http) or permission refused: fall back to copying the selected text.
    secretInput.value?.select();
    copyState.value = document.execCommand("copy") ? "copied" : "failed";
  }
}
</script>

<template>
  <dialog ref="dialog" class="confirm form-dialog" aria-labelledby="token-dialog-title" @cancel="close">
    <form v-if="!created" novalidate @submit.prevent="submit">
      <h2 id="token-dialog-title">New API token</h2>
      <div v-if="open" class="body">
        <div v-if="forbidden" class="alert alert-error" role="alert">
          <strong>Not created — you cannot create a token for this owner.</strong>
          <div>
            The owner holds permissions you do not have, and a token acts as its owner. Choose another owner, or ask an
            administrator to create it.
          </div>
          <div class="meta">{{ apiError?.message }}</div>
        </div>
        <FormErrorBanner v-else-if="create.error.value" :error="create.error.value" :unplaced="unplaced" />
        <div class="form-grid">
          <FormField id="token-name" v-slot="p" label="Name" required wide :error="errorFor('name')" hint="What uses the token, e.g. “backup script” or “monitoring”">
            <input
              :id="p.id"
              ref="nameInput"
              v-model="name"
              type="text"
              maxlength="200"
              autocomplete="off"
              :aria-invalid="p.invalid || undefined"
              :aria-describedby="p.describedBy"
            />
          </FormField>
          <FormField
            id="token-owner"
            v-slot="p"
            label="Owner"
            :error="errorFor('userId')"
            :hint="moreUsers ? 'The token acts as this user. Only the first 200 active users are listed.' : 'The token acts as this user. Use a dedicated user for a service.'"
          >
            <select :id="p.id" v-model="userId" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
              <option value="">Me ({{ session.user?.username }})</option>
              <option v-for="u in ownerOptions" :key="u.id" :value="u.id">{{ u.username }} — {{ u.displayName }}</option>
            </select>
          </FormField>
          <FormField id="token-profile" v-slot="p" label="Permission profile" required :error="errorFor('profileId')" hint="The token may do only what both this profile and its owner allow">
            <select :id="p.id" v-model="profileId" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
              <option value="">{{ profiles.isLoading.value ? "Loading…" : "Choose a profile…" }}</option>
              <option v-for="pr in profiles.data.value?.data ?? []" :key="pr.id" :value="pr.id">{{ pr.name }}</option>
            </select>
          </FormField>
          <FormField id="token-expiry" v-slot="p" label="Expires" required :error="expiry === 'custom' ? undefined : errorFor('expiresAt')" :hint="expiresAt && expiry !== 'custom' ? `On ${formatDateTime(expiresAt.toISOString())}` : undefined">
            <select :id="p.id" v-model="expiry" :aria-invalid="(expiry !== 'custom' && p.invalid) || undefined" :aria-describedby="p.describedBy">
              <option v-for="pr in PRESETS" :key="pr.days" :value="String(pr.days)">In {{ pr.label }}</option>
              <option value="custom">On a date…</option>
            </select>
          </FormField>
          <FormField v-if="expiry === 'custom'" id="token-expiry-date" v-slot="p" label="Expiry date" required :error="errorFor('expiresAt')" hint="At the end of that day, at most a year ahead">
            <input
              :id="p.id"
              v-model="customDate"
              type="date"
              :min="minDate"
              :max="maxDate"
              :aria-invalid="p.invalid || undefined"
              :aria-describedby="p.describedBy"
            />
          </FormField>
        </div>
      </div>
      <div class="footer">
        <button type="button" class="btn" :disabled="create.isPending.value" @click="close()">Cancel</button>
        <button type="submit" class="btn btn-primary" :disabled="create.isPending.value">
          {{ create.isPending.value ? "Creating…" : "Create token" }}
        </button>
      </div>
    </form>

    <div v-else>
      <h2 id="token-dialog-title">API token “{{ created.token.name }}” created</h2>
      <div class="body stack">
        <div class="alert alert-warn" role="alert">
          <strong>Copy the secret now. You won't see it again.</strong>
          <div>ShadouCMDB keeps only a hash of it. If it is lost, revoke this token and create a new one.</div>
        </div>
        <div class="field">
          <label for="token-secret">Secret</label>
          <div class="copy-row">
            <input id="token-secret" ref="secretInput" class="mono" type="text" readonly spellcheck="false" autocomplete="off" :value="created.secret" @focus="secretInput?.select()" />
            <button type="button" class="btn btn-primary" @click="copySecret">Copy</button>
          </div>
          <span class="hint">Send it as <code>Authorization: Bearer &lt;secret&gt;</code>.</span>
          <span role="status" :class="copyState === 'failed' ? 'error' : 'hint'">
            {{ copyState === "copied" ? "Copied to the clipboard." : copyState === "failed" ? "Could not copy — select the secret and copy it by hand." : "" }}
          </span>
        </div>
        <dl class="props">
          <dt>Owner</dt>
          <dd>{{ created.token.username }}</dd>
          <dt>Profile</dt>
          <dd>{{ created.token.profile?.name }}</dd>
          <dt>Expires</dt>
          <dd>{{ formatDateTime(created.token.expiresAt) }}</dd>
        </dl>
      </div>
      <div class="footer">
        <button type="button" class="btn btn-primary" @click="close()">Done</button>
      </div>
    </div>
  </dialog>
</template>

<style scoped>
.copy-row {
  display: flex;
  gap: var(--sp-2);
}
.copy-row input {
  flex: 1;
  min-width: 0;
}
</style>
