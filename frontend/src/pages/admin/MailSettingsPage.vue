<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { ApiError } from "../../api/client";
import { useMailStatus, useSendMailTest } from "../../api/webhooks";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { t } from "../../i18n";
import { useDocumentTitle } from "../../lib/composables";
import { formatDateTime, formatRelative } from "../../lib/format";
import { useSessionStore } from "../../stores/session";
import { adminCrumbs } from "./sections";

/**
 * Administration › Outbound e-mail (SHAA-2725 §6.1): read-only, since the operator sets mail in the
 * environment (MAIL, SMTP_*, MAIL_*). Shows the relay, its security and the sender (never the user name or
 * password) and the last outcomes of the server process that answered; sends a test message to the
 * caller's own address.
 */
useDocumentTitle(() => t("admin.section.mail"));
const session = useSessionStore();
const status = useMailStatus();
const test = useSendMailTest();
const s = computed(() => status.data.value);
const notConfigured = computed(() => test.error.value instanceof ApiError && test.error.value.code === "MAIL_NOT_CONFIGURED");
const LOCALES: Record<string, string> = { en: "English", de: "Deutsch" };
const securityLabel = (v: string | null | undefined) =>
  v === "starttls" ? "STARTTLS" : v === "tls" ? t("mail.security.tls") : v === "none" ? t("mail.security.none") : (v ?? "");

function send() {
  test.reset();
  test.mutate();
}
</script>

<template>
  <div class="record-head record-head-plain">
    <Breadcrumbs :items="adminCrumbs('mail')" />
    <div class="page-header">
      <div class="title">
        <h1>{{ t("admin.section.mail") }}</h1>
      </div>
    </div>
    <p class="page-intro">{{ t("mail.intro") }}</p>
  </div>

  <div class="mail-page">
    <ErrorAlert v-if="status.isError.value" :error="status.error.value" :on-retry="() => status.refetch()" />
    <p v-else-if="status.isPending.value" class="muted">{{ t("common.loading") }}</p>
    <template v-else-if="s">
      <section class="panel" aria-labelledby="mail-status-title">
        <div class="panel-header"><h2 id="mail-status-title">{{ t("mail.status.title") }}</h2></div>
        <div class="panel-body stack">
          <div v-if="!s.enabled" class="alert alert-warn" role="status" data-testid="mail-off">
            <strong>{{ t("mail.off.title") }}</strong>
            <div>{{ t("mail.off.body") }}</div>
          </div>
          <div v-else-if="s.lastError" class="alert alert-error" role="status">
            <strong>{{ t("mail.lastError", { when: formatDateTime(s.lastErrorAt) }) }}</strong>
            <div class="mono">{{ s.lastError }}</div>
          </div>
          <dl class="props" data-testid="mail-status">
            <dt>{{ t("mail.enabled") }}</dt>
            <dd>
              <span :class="['badge', s.enabled ? 'ok' : 'off']"><span class="status-dot" aria-hidden="true" />{{ s.enabled ? t("mail.on") : t("mail.offShort") }}</span>
            </dd>
            <template v-if="s.enabled">
              <dt>{{ t("mail.relay") }}</dt>
              <dd class="mono">{{ s.host }}<template v-if="s.port">:{{ s.port }}</template></dd>
              <dt>{{ t("mail.security") }}</dt>
              <dd>
                {{ securityLabel(s.security) }}
                <span v-if="s.security === 'none'" class="badge warn">{{ t("webhooks.unencrypted") }}</span>
              </dd>
              <dt>{{ t("mail.from") }}</dt>
              <dd class="mono">{{ s.from }}</dd>
            </template>
            <dt>{{ t("mail.defaultLocale") }}</dt>
            <dd>{{ LOCALES[s.defaultLocale] ?? s.defaultLocale }}</dd>
            <dt>{{ t("mail.externalAddresses") }}</dt>
            <dd>{{ s.externalAddresses ? t("mail.externalAllowed") : t("mail.externalRefused") }}</dd>
            <dt>{{ t("mail.perRecipient") }}</dt>
            <dd>{{ t("mail.perRecipientValue", { n: s.maxPerRecipientPerHour }) }}</dd>
            <template v-if="s.enabled">
              <dt>{{ t("mail.lastSuccess") }}</dt>
              <dd>
                <time v-if="s.lastSuccessAt" :datetime="s.lastSuccessAt" :title="formatDateTime(s.lastSuccessAt)">{{ formatRelative(s.lastSuccessAt) }}</time>
                <span v-else class="muted">{{ t("mail.noSuccessYet") }}</span>
              </dd>
            </template>
          </dl>
          <p class="muted no-margin">{{ t("mail.processNote") }}</p>
        </div>
      </section>

      <section class="panel" aria-labelledby="mail-test-title">
        <div class="panel-header"><h2 id="mail-test-title">{{ t("mail.test.title") }}</h2></div>
        <div class="panel-body stack">
          <p class="muted no-margin">{{ t("mail.test.intro", { address: session.user?.email ?? "" }) }}</p>
          <div aria-live="polite">
            <div v-if="notConfigured" class="alert alert-warn" role="alert">
              <strong>{{ t("mail.off.title") }}</strong>
              <div>{{ t("mail.off.body") }}</div>
            </div>
            <ErrorAlert v-else-if="test.isError.value" :error="test.error.value" :title="t('mail.test.failed')" />
            <template v-else-if="test.data.value">
              <div v-if="test.data.value.sent" class="alert alert-success" role="status" data-testid="mail-test-result">
                <strong>{{ t("mail.test.sent", { to: test.data.value.to }) }}</strong>
                <div>{{ t("mail.test.sentBody") }}</div>
              </div>
              <div v-else class="alert alert-error" role="alert" data-testid="mail-test-result">
                <strong>{{ t("mail.test.refused", { to: test.data.value.to }) }}</strong>
                <div class="mono">
                  <template v-if="test.data.value.smtpCode">{{ test.data.value.smtpCode }} </template>{{ test.data.value.error }}
                </div>
              </div>
            </template>
          </div>
          <div>
            <button type="button" class="btn btn-primary" :disabled="test.isPending.value || !s.enabled" data-testid="mail-test" @click="send">
              {{ test.isPending.value ? t("mail.test.sending") : t("mail.test.submit") }}
            </button>
          </div>
          <p class="muted no-margin">
            {{ t("mail.test.languageNote") }} <RouterLink to="/account">{{ t("account.title") }}</RouterLink>
          </p>
        </div>
      </section>
    </template>
  </div>
</template>

<style scoped>
.mail-page {
  max-width: 880px;
}
</style>
