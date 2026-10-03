<script setup lang="ts">
import { computed, ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import BrandMark from "../../components/BrandMark.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { t } from "../../i18n";
import { useDocumentTitle } from "../../lib/composables";
import { vAutofocus } from "../../lib/directives";
import { emailErrorMessage, looksLikeEmail } from "../../lib/people";
import { safeRedirect } from "../../lib/signIn";
import { useSessionStore } from "../../stores/session";
import FormField from "../form/FormField.vue";

/**
 * Forced e-mail step (SHAA-1505 decision 9): the account was created before e-mail addresses were required and has
 * none, so it is not linked to a Person CI yet. Until it enters one, the API answers every other route with
 * 403 EMAIL_REQUIRED, so this screen stands alone (no navigation, no search) and offers only the e-mail and sign-out.
 * Saving links the account to the Person CI with that e-mail (created when there is none), then the user goes on to
 * where they were going (by way of the two-factor set-up, when a profile requires it).
 */
useDocumentTitle(() => t("people.entry.documentTitle"));
const route = useRoute();
const router = useRouter();
const session = useSessionStore();
const email = ref("");
const busy = ref(false);
const error = ref<unknown>(null);
const local = ref<string | undefined>();
const signOutError = ref<unknown>(null);

/** A refused address is told next to the field; anything else (a lost connection, a server error) above the form. */
const fieldError = computed(() => local.value ?? (error.value instanceof ApiError ? emailErrorMessage(error.value.details, true) : undefined));
const otherError = computed(() => (error.value && !fieldError.value ? error.value : null));

async function submit() {
  error.value = null;
  const value = email.value.trim();
  local.value = !value ? t("people.entry.required") : !looksLikeEmail(value) ? t("people.entry.invalid") : undefined;
  if (local.value) {
    document.getElementById("email-entry")?.focus();
    return;
  }
  busy.value = true;
  try {
    await session.enterEmail(value);
    await router.replace(safeRedirect(route.query.redirect));
  } catch (e) {
    error.value = e;
    if (fieldError.value) document.getElementById("email-entry")?.focus();
    // 409 without a field: the account has an e-mail by now (entered in another tab). Go on with it.
    else if (e instanceof ApiError && e.status === 409) {
      await session.refresh().catch(() => undefined);
      if (!session.emailRequired) await router.replace(safeRedirect(route.query.redirect));
    }
  } finally {
    busy.value = false;
  }
}

/** Back to the sign-in screen, which returns to where the user was going. */
async function signOut() {
  signOutError.value = null;
  try {
    await session.logout();
    const redirect = safeRedirect(route.query.redirect);
    await router.replace({ path: "/login", query: redirect === "/" ? {} : { redirect } });
  } catch (e) {
    signOutError.value = e;
  }
}
</script>

<template>
  <main class="bare">
    <form class="bare-card" aria-labelledby="email-entry-title" novalidate data-testid="email-entry" @submit.prevent="submit">
      <div class="bare-brand"><BrandMark /></div>
      <h1 id="email-entry-title">{{ t("people.entry.title") }}</h1>
      <p v-if="session.user">
        {{ t("people.entry.signedInAs", { name: session.user.displayName, username: session.user.username }) }}
      </p>
      <p>{{ t("people.entry.intro") }}</p>
      <ErrorAlert v-if="otherError" :error="otherError" :title="t('people.entry.failed')" />
      <ErrorAlert v-if="signOutError" :error="signOutError" :title="t('account.enrol.signOutFailed')" />
      <FormField id="email-entry" :label="t('people.entry.label')" required :error="fieldError" :hint="t('people.entry.hint')">
        <template #default="{ id: fid, invalid, describedBy }">
          <input
            :id="fid"
            v-model="email"
            v-autofocus
            type="email"
            autocomplete="email"
            maxlength="254"
            spellcheck="false"
            :aria-invalid="invalid"
            :aria-describedby="describedBy"
          />
        </template>
      </FormField>
      <button type="submit" class="btn btn-primary block" :disabled="busy">{{ busy ? t("people.entry.saving") : t("people.entry.submit") }}</button>
      <button type="button" class="btn block" @click="signOut">{{ t("account.enrol.signOut") }}</button>
    </form>
  </main>
</template>
