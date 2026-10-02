<script setup lang="ts">
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import { t } from "../../i18n";
import { useDocumentTitle } from "../../lib/composables";
import { useSessionStore } from "../../stores/session";
import PasswordSettings from "./PasswordSettings.vue";
import TwoFactorSettings from "./TwoFactorSettings.vue";

/** The signed-in user's own account: who they are, their password and their two-factor authentication. */
useDocumentTitle(() => t("account.title"));
const session = useSessionStore();
</script>

<template>
  <Breadcrumbs :items="[{ label: t('account.title') }]" />
  <div class="page-header">
    <div class="title">
      <h1>{{ t("account.title") }}</h1>
      <span v-if="session.user" class="muted">{{ session.user.displayName }} ({{ session.user.username }})</span>
    </div>
  </div>
  <div class="account">
    <PasswordSettings />
    <TwoFactorSettings />
  </div>
</template>

<style scoped>
.account {
  max-width: 880px;
}
</style>
