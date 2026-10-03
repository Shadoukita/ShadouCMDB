<script setup lang="ts">
import { computed } from "vue";
import { RouterLink } from "vue-router";
import { useIsPersonClass, useSignInAccount } from "../../api/admin";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { t } from "../../i18n";
import { useSessionStore } from "../../stores/session";

/**
 * "Sign-in account" on a Person CI's Overview (SHAA-1505 decision 11): the user account linked to this person, with a
 * link to it. For those who manage user accounts only; everyone else sees the person as any other CI. A deleted
 * Person has no account (deleting is refused while one is linked).
 */
const props = defineProps<{ ci: { id: string; classId: string; deletedAt?: string | null } }>();
const session = useSessionStore();
const isPerson = useIsPersonClass(() => props.ci.classId);
const shown = computed(() => isPerson.value && !props.ci.deletedAt && session.can("users.manage"));
const answer = useSignInAccount(() => props.ci.id, shown);
const account = computed(() => answer.data.value?.account ?? null);
</script>

<template>
  <section v-if="shown" class="panel sign-in-account" aria-labelledby="sign-in-account-title" :aria-busy="answer.isPending.value" data-testid="sign-in-account">
    <div class="panel-header"><h2 id="sign-in-account-title">{{ t("people.panel.title") }}</h2></div>
    <div class="panel-body">
      <ErrorAlert v-if="answer.isError.value" :error="answer.error.value" :title="t('people.panel.failed')" :on-retry="() => answer.refetch()" />
      <p v-else-if="answer.isPending.value" class="muted">{{ t("people.panel.loading") }}</p>
      <template v-else-if="account">
        <dl class="props">
          <dt>{{ t("people.panel.username") }}</dt>
          <dd>
            <RouterLink v-if="account.userId" :to="`/admin/users/${account.userId}`" data-testid="sign-in-account-link">{{ account.username }}</RouterLink>
            <template v-else>{{ account.username }}</template>
          </dd>
          <dt>{{ t("people.panel.displayName") }}</dt>
          <dd dir="auto">{{ account.displayName }}</dd>
          <dt>{{ t("people.panel.status") }}</dt>
          <dd>
            <span v-if="account.isActive" class="badge ok">{{ t("people.panel.active") }}</span>
            <span v-else class="badge off">{{ t("people.panel.disabled") }}</span>
          </dd>
        </dl>
        <p class="hint">{{ t("people.panel.managed") }}</p>
      </template>
      <p v-else class="muted" data-testid="sign-in-account-none">{{ t("people.panel.none") }}</p>
    </div>
  </section>
</template>
