<script setup lang="ts">
import { computed } from "vue";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import Icon from "../../components/Icon.vue";
import { t } from "../../i18n";
import { useDocumentTitle } from "../../lib/composables";
import { useSessionStore } from "../../stores/session";
import PasswordSettings from "./PasswordSettings.vue";
import TwoFactorSettings from "./TwoFactorSettings.vue";

/** The signed-in user's own account: who they are, their password and their two-factor authentication. */
useDocumentTitle(() => t("account.title"));
const session = useSessionStore();
const user = computed(() => session.user);
</script>

<template>
  <div class="record-head record-head-plain">
    <Breadcrumbs :items="[{ label: t('account.title') }]" />
    <div class="page-header record-header">
      <div class="record-heading">
        <span class="class-tile class-tile-lg" aria-hidden="true"><Icon name="user" class="class-icon" /></span>
        <div class="record-title">
          <div class="title">
            <h1>{{ t("account.title") }}</h1>
          </div>
          <p v-if="user" class="record-meta" data-testid="record-meta">
            <span v-if="user.isAdministrator" class="badge">{{ t("admin.user.administrator") }}</span>
            <span v-if="user.identityProvider" class="badge" data-testid="account-provider">{{ t("admin.user.signsInWith", { name: user.identityProvider.name }) }}</span>
            <span v-else class="badge">{{ t("account.localAccount") }}</span>
            <span class="record-meta-line">
              <span dir="auto">{{ user.displayName }}</span>
              <span class="sep" aria-hidden="true">·</span>
              <span class="mono">{{ user.username }}</span>
              <template v-if="user.email">
                <span class="sep" aria-hidden="true">·</span>
                <span class="mono">{{ user.email }}</span>
              </template>
            </span>
          </p>
        </div>
      </div>
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
