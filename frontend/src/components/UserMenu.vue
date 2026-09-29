<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
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
const THEME_NAMES = { light: "Light", dark: "Dark", system: "System" } as const;
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
      :title="`Signed in as ${session.user.username}`"
      @click="open = !open"
    >
      <span class="sr-only">Signed in as</span>
      <span class="who-name">{{ session.user.displayName }}</span>
      <span aria-hidden="true">▾</span>
    </button>
    <div v-show="!compact || open" id="user-menu-panel" class="user-menu-panel">
      <RouterLink class="who" to="/account" :title="`Signed in as ${session.user.username}: my account and two-factor authentication`">
        <template v-if="compact">My account</template>
        <template v-else>
          <span class="sr-only">Signed in as</span>
          <span class="who-name">{{ session.user.displayName }}</span>
          <span v-if="session.user.isAdministrator" class="badge">Administrator</span>
        </template>
      </RouterLink>
      <label :class="compact ? 'menu-label' : 'sr-only'" for="user-theme">Theme</label>
      <select
        id="user-theme"
        class="theme-select"
        title="Theme"
        :value="branding.userTheme ?? ''"
        @change="branding.setUserTheme((($event.target as HTMLSelectElement).value || null) as 'light' | 'dark' | 'system' | null)"
      >
        <option value="">{{ compact ? "" : "Theme: " }}default ({{ THEME_NAMES[branding.effective.defaultTheme] }})</option>
        <option value="light">{{ compact ? "" : "Theme: " }}light</option>
        <option value="dark">{{ compact ? "" : "Theme: " }}dark</option>
        <option value="system">{{ compact ? "" : "Theme: " }}follow system</option>
      </select>
      <button type="button" class="btn" :disabled="busy" @click="signOut">{{ busy ? "Signing out…" : "Sign out" }}</button>
    </div>
    <div v-if="error" class="user-menu-error"><ErrorAlert :error="error" title="Sign-out failed" /></div>
  </div>
</template>
