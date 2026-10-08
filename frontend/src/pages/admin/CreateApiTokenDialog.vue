<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useAllProfiles, useCreateApiToken, useUserList, type ApiToken } from "../../api/admin";
import { ApiError } from "../../api/client";
import { MAX_PAGE } from "../../api/queries";
import Icon from "../../components/Icon.vue";
import { t } from "../../i18n";
import { formatDateTime } from "../../lib/format";
import { useSessionStore } from "../../stores/session";
import FormErrorBanner from "../form/FormErrorBanner.vue";
import FormField from "../form/FormField.vue";

/**
 * New API token: name, owner, profile and expiry, then the secret, shown once, in the data font with a
 * copy button (design §2.7 dialogs).
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
const PRESETS = [7, 30, 90, 180, 365];
const presetLabel = (days: number) => (days === 365 ? t("admin.token.inYear") : t("admin.token.inDays", { n: days }));

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
/** 403: the owner must use two-factor authentication and this session did not sign in with a second factor (GH#200). */
const mfaRequired = computed(() => apiError.value?.code === "MFA_REQUIRED_FOR_TOKEN");

function submit() {
  const errs: Record<string, string> = {};
  if (!name.value.trim()) errs.name = t("admin.token.nameRequired");
  if (!profileId.value) errs.profileId = t("admin.token.profileRequired");
  if (!expiresAt.value) errs.expiresAt = t("admin.token.expiryRequired");
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
  <dialog ref="dialog" class="confirm form-dialog token-dialog" aria-labelledby="token-dialog-title" @cancel="close">
    <form v-if="!created" novalidate @submit.prevent="submit">
      <h2 id="token-dialog-title">{{ t("admin.token.newTitle") }}</h2>
      <div v-if="open" class="body">
        <div v-if="forbidden" class="alert alert-error" role="alert">
          <strong>{{ t("admin.token.forbiddenTitle") }}</strong>
          <div>{{ t("admin.token.forbiddenBody") }}</div>
          <div class="meta">{{ apiError?.message }}</div>
        </div>
        <div v-else-if="mfaRequired" class="alert alert-error" role="alert">
          <strong>{{ t("admin.token.mfaTitle") }}</strong>
          <div>{{ apiError?.message }}</div>
          <div><RouterLink to="/account" @click="close()">{{ t("admin.token.mfaLink") }}</RouterLink> {{ t("admin.token.mfaAfterLink") }}</div>
        </div>
        <FormErrorBanner v-else-if="create.error.value" :error="create.error.value" :unplaced="unplaced" />
        <div class="form-grid">
          <FormField id="token-name" v-slot="p" :label="t('admin.token.name')" required wide :error="errorFor('name')" :hint="t('admin.token.nameHint')">
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
            :label="t('admin.token.owner')"
            :error="errorFor('userId')"
            :hint="moreUsers ? t('admin.token.ownerHintMore') : t('admin.token.ownerHint')"
          >
            <select :id="p.id" v-model="userId" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
              <option value="">{{ t("admin.token.me", { name: session.user?.username ?? "" }) }}</option>
              <option v-for="u in ownerOptions" :key="u.id" :value="u.id">{{ u.username }} — {{ u.displayName }}</option>
            </select>
          </FormField>
          <FormField id="token-profile" v-slot="p" :label="t('admin.token.profile')" required :error="errorFor('profileId')" :hint="t('admin.token.profileHint')">
            <select :id="p.id" v-model="profileId" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
              <option value="">{{ profiles.isLoading.value ? t("common.loading") : t("admin.token.chooseProfile") }}</option>
              <option v-for="pr in profiles.data.value?.data ?? []" :key="pr.id" :value="pr.id">{{ pr.name }}</option>
            </select>
          </FormField>
          <FormField
            id="token-expiry"
            v-slot="p"
            :label="t('admin.token.expires')"
            required
            :error="expiry === 'custom' ? undefined : errorFor('expiresAt')"
            :hint="expiresAt && expiry !== 'custom' ? t('admin.token.expiresOn', { when: formatDateTime(expiresAt.toISOString()) }) : undefined"
          >
            <select :id="p.id" v-model="expiry" :aria-invalid="(expiry !== 'custom' && p.invalid) || undefined" :aria-describedby="p.describedBy">
              <option v-for="d in PRESETS" :key="d" :value="String(d)">{{ presetLabel(d) }}</option>
              <option value="custom">{{ t("admin.token.onDate") }}</option>
            </select>
          </FormField>
          <FormField
            v-if="expiry === 'custom'"
            id="token-expiry-date"
            v-slot="p"
            :label="t('admin.token.expiryDate')"
            required
            :error="errorFor('expiresAt')"
            :hint="t('admin.token.expiryDateHint')"
          >
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
        <button type="button" class="btn" :disabled="create.isPending.value" @click="close()">{{ t("common.cancel") }}</button>
        <button type="submit" class="btn btn-primary" :disabled="create.isPending.value">
          {{ create.isPending.value ? t("admin.token.creating") : t("admin.token.create") }}
        </button>
      </div>
    </form>

    <template v-else>
      <h2 id="token-dialog-title">{{ t("admin.token.createdTitle", { name: created.token.name }) }}</h2>
      <div class="body stack">
        <div class="alert alert-warn" role="alert">
          <strong>{{ t("admin.token.copyNow") }}</strong>
          <div>{{ t("admin.token.copyNowBody") }}</div>
        </div>
        <div class="field">
          <label for="token-secret">{{ t("admin.token.secret") }}</label>
          <div class="token-copy-row">
            <input id="token-secret" ref="secretInput" class="mono" type="text" readonly spellcheck="false" autocomplete="off" :value="created.secret" @focus="secretInput?.select()" />
            <button type="button" class="btn" @click="copySecret">
              <Icon :name="copyState === 'copied' ? 'check' : 'copy'" :size="16" />{{ t("admin.token.copy") }}
            </button>
          </div>
          <span class="hint">{{ t("admin.token.sendAs") }} <code>Authorization: Bearer &lt;secret&gt;</code></span>
          <span role="status" :class="copyState === 'failed' ? 'error' : 'hint'">
            {{ copyState === "copied" ? t("admin.token.copied") : copyState === "failed" ? t("admin.token.copyFailed") : "" }}
          </span>
        </div>
        <dl class="props">
          <dt>{{ t("admin.token.owner") }}</dt>
          <dd class="mono">{{ created.token.username }}</dd>
          <dt>{{ t("admin.token.profileShort") }}</dt>
          <dd>{{ created.token.profile?.name }}</dd>
          <dt>{{ t("admin.token.expires") }}</dt>
          <dd>{{ formatDateTime(created.token.expiresAt) }}</dd>
        </dl>
      </div>
      <div class="footer">
        <button type="button" class="btn btn-primary" @click="close()">{{ t("admin.token.done") }}</button>
      </div>
    </template>
  </dialog>
</template>
