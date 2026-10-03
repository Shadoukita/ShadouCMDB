<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { t } from "../i18n";
import { useBrandingStore } from "../stores/branding";
import { DENSITIES, useDensityStore, type Density } from "../stores/density";
import { useSessionStore } from "../stores/session";
import ErrorAlert from "./ErrorAlert.vue";
import Icon from "./Icon.vue";

/**
 * The acting user in the header: initials and name on a button that opens a panel with My account, the
 * theme and density choices and "Sign out" (design document §2.7, audit S5). Below 960 px the name folds
 * away and only the initials show; the name stays the button's accessible name.
 */
const session = useSessionStore();
const router = useRouter();
const route = useRoute();
const open = ref(false);
const root = ref<HTMLElement>();
const toggle = ref<HTMLButtonElement>();
watch(() => route.fullPath, () => (open.value = false));
function onDocClick(e: MouseEvent) {
  if (open.value && !root.value?.contains(e.target as Node)) open.value = false;
}
function onKeydown(e: KeyboardEvent) {
  if (e.key === "Escape" && open.value) {
    e.stopPropagation();
    open.value = false;
    toggle.value?.focus();
  }
}
onMounted(() => document.addEventListener("click", onDocClick));
onBeforeUnmount(() => document.removeEventListener("click", onDocClick));
const branding = useBrandingStore();
const density = useDensityStore();
const initials = computed(() => {
  const name = session.user?.displayName?.trim() || session.user?.username || "";
  const words = name.split(/\s+/).filter(Boolean);
  return words
    .slice(0, 2)
    .map((w) => Array.from(w)[0])
    .join("")
    .toLocaleUpperCase();
});
const busy = ref(false);
const error = ref<unknown>(null);

async function signOut() {
  busy.value = true;
  error.value = null;
  try {
    await session.logout();
    await router.replace("/login");
  } catch (e) {
    error.value = e;
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div v-if="session.user" ref="root" class="user-menu" @keydown="onKeydown">
    <button
      ref="toggle"
      type="button"
      class="who"
      aria-haspopup="true"
      aria-controls="user-menu-panel"
      :aria-expanded="open"
      :title="t('userMenu.signedInAsName', { name: session.user.username })"
      @click="open = !open"
    >
      <span class="sr-only">{{ t("userMenu.signedInAs") }}</span>
      <span class="who-initials" aria-hidden="true">{{ initials }}</span>
      <span class="who-name">{{ session.user.displayName }}</span>
      <Icon name="chevron-down" class="who-chevron" />
    </button>
    <div v-show="open" id="user-menu-panel" class="user-menu-panel">
      <div class="menu-who">
        <span class="menu-who-name">{{ t("userMenu.signedInAsName", { name: session.user.displayName }) }}</span>
        <span class="menu-who-username mono">{{ session.user.username }}</span>
        <span v-if="session.user.isAdministrator" class="badge">{{ t("userMenu.administrator") }}</span>
      </div>
      <RouterLink class="menu-link" to="/account" :title="t('userMenu.accountTitle', { name: session.user.username })">
        <Icon name="user" />{{ t("account.title") }}
      </RouterLink>
      <div class="menu-field">
        <label for="user-theme">{{ t("userMenu.theme") }}</label>
        <select
          id="user-theme"
          :value="branding.userTheme ?? ''"
          @change="branding.setUserTheme((($event.target as HTMLSelectElement).value || null) as 'light' | 'dark' | 'system' | null)"
        >
          <option value="">{{ t("userMenu.theme.default", { theme: t(`userMenu.theme.name.${branding.effective.defaultTheme}`) }) }}</option>
          <option value="light">{{ t("userMenu.theme.light") }}</option>
          <option value="dark">{{ t("userMenu.theme.dark") }}</option>
          <option value="system">{{ t("userMenu.theme.system") }}</option>
        </select>
      </div>
      <div class="menu-field">
        <label for="user-density">{{ t("userMenu.density") }}</label>
        <select
          id="user-density"
          :value="density.density"
          @change="density.setDensity(($event.target as HTMLSelectElement).value as Density)"
        >
          <option v-for="d in DENSITIES" :key="d" :value="d">{{ t(`userMenu.density.${d}`) }}</option>
        </select>
      </div>
      <button type="button" class="btn menu-sign-out" :disabled="busy" @click="signOut">
        <Icon name="log-out" />{{ busy ? t("userMenu.signingOut") : t("userMenu.signOut") }}
      </button>
    </div>
    <div v-if="error" class="user-menu-error"><ErrorAlert :error="error" :title="t('userMenu.signOutFailed')" /></div>
  </div>
</template>
