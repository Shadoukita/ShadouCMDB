<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { t } from "../i18n";
import { useMediaQuery } from "../lib/composables";
import { useBrandingStore } from "../stores/branding";
import { useSessionStore } from "../stores/session";
import ErrorAlert from "./ErrorAlert.vue";

/**
 * The acting user, theme and "Sign out" in the header. Below 960 px they fold into a
 * menu behind the user's name so the search field and "New CI" keep their room.
 */
const session = useSessionStore();
const router = useRouter();
const route = useRoute();
const compact = useMediaQuery("(max-width: 960px)");
const open = ref(false);
const root = ref<HTMLElement>();
const toggle = ref<HTMLButtonElement>();
watch([() => route.fullPath, compact], () => (open.value = false));
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
/** An option's text: "Theme: light" in the header's select, just "light" in the folded menu under its "Theme" label. */
const themeOption = (name: string) => (compact.value ? name : t("userMenu.theme.option", { name }));
/** Shown instead of the name on phone-width screens (GH#363); the name stays the button's accessible name. */
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
  <div v-if="session.user" ref="root" class="user-menu" :class="{ compact }" @keydown="onKeydown">
    <button
      v-if="compact"
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
      <span class="who-name">{{ session.user.displayName }}</span>
      <span class="who-initials" aria-hidden="true">{{ initials }}</span>
      <span aria-hidden="true">▾</span>
    </button>
    <div v-show="!compact || open" id="user-menu-panel" class="user-menu-panel">
      <span v-if="compact" class="menu-label menu-who">{{ t("userMenu.signedInAsName", { name: session.user.displayName }) }}</span>
      <RouterLink class="who" to="/account" :title="t('userMenu.accountTitle', { name: session.user.username })">
        <template v-if="compact">{{ t("account.title") }}</template>
        <template v-else>
          <span class="sr-only">{{ t("userMenu.signedInAs") }}</span>
          <span class="who-name">{{ session.user.displayName }}</span>
          <span v-if="session.user.isAdministrator" class="badge">{{ t("userMenu.administrator") }}</span>
        </template>
      </RouterLink>
      <label :class="compact ? 'menu-label' : 'sr-only'" for="user-theme">{{ t("userMenu.theme") }}</label>
      <select
        id="user-theme"
        class="theme-select"
        :title="t('userMenu.theme')"
        :value="branding.userTheme ?? ''"
        @change="branding.setUserTheme((($event.target as HTMLSelectElement).value || null) as 'light' | 'dark' | 'system' | null)"
      >
        <option value="">{{ themeOption(t("userMenu.theme.default", { theme: t(`userMenu.theme.name.${branding.effective.defaultTheme}`) })) }}</option>
        <option value="light">{{ themeOption(t("userMenu.theme.light")) }}</option>
        <option value="dark">{{ themeOption(t("userMenu.theme.dark")) }}</option>
        <option value="system">{{ themeOption(t("userMenu.theme.system")) }}</option>
      </select>
      <button type="button" class="btn" :disabled="busy" @click="signOut">{{ busy ? t("userMenu.signingOut") : t("userMenu.signOut") }}</button>
    </div>
    <div v-if="error" class="user-menu-error"><ErrorAlert :error="error" :title="t('userMenu.signOutFailed')" /></div>
  </div>
</template>
