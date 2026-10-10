<script setup lang="ts">
import { computed, ref, watch } from "vue";
import type { Session } from "../../api/admin";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { t } from "../../i18n";
import { useSessionStore } from "../../stores/session";
import FormField from "../form/FormField.vue";

/**
 * The language of the e-mails workflow actions send this user (SHAA-2725 §6.4), written with PATCH /auth/me.
 * The web UI itself is English; "Server default" leaves it to the operator's MAIL_DEFAULT_LOCALE.
 */
type Choice = "" | NonNullable<Session["locale"]>;
const session = useSessionStore();
const stored = computed<Choice>(() => session.session?.locale ?? "");
const choice = ref<Choice>(stored.value);
watch(stored, (v) => (choice.value = v));

const saving = ref(false);
const saved = ref(false);
const error = ref<unknown>(null);
const dirty = computed(() => choice.value !== stored.value);

async function submit() {
  saved.value = false;
  error.value = null;
  saving.value = true;
  try {
    await session.setMailLocale(choice.value || null);
    saved.value = true;
  } catch (e) {
    error.value = e;
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <section class="panel" aria-labelledby="mail-locale-title">
    <div class="panel-header"><h2 id="mail-locale-title">{{ t("account.mailLocale.title") }}</h2></div>
    <form novalidate @submit.prevent="submit">
      <div class="panel-body stack">
        <p class="muted no-margin">{{ t("account.mailLocale.intro") }}</p>
        <div v-if="saved && !dirty" class="alert alert-success" role="status">{{ t("account.mailLocale.saved") }}</div>
        <ErrorAlert v-if="error" :error="error" :title="t('account.mailLocale.failed')" />
        <div class="form-grid">
          <FormField id="own-mail-locale" v-slot="p" :label="t('account.mailLocale.label')">
            <select :id="p.id" v-model="choice" :aria-describedby="p.describedBy" data-testid="mail-locale">
              <option value="">{{ t("account.mailLocale.default") }}</option>
              <option value="en">English</option>
              <option value="de">Deutsch</option>
            </select>
          </FormField>
        </div>
      </div>
      <div class="form-footer">
        <button type="submit" class="btn btn-primary" :disabled="saving || !dirty">{{ saving ? t("common.saving") : t("account.mailLocale.submit") }}</button>
      </div>
    </form>
  </section>
</template>
