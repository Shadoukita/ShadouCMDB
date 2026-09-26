<script setup lang="ts">
import { ref } from "vue";
import { useRouter } from "vue-router";
import { useBrandingStore } from "../stores/branding";
import { useSessionStore } from "../stores/session";
import ErrorAlert from "./ErrorAlert.vue";

/** The acting user and "Sign out" in the header. */
const session = useSessionStore();
const router = useRouter();
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
  <div v-if="session.user" class="user-menu">
    <span class="who" :title="`Signed in as ${session.user.username}`">
      <span class="sr-only">Signed in as</span>
      {{ session.user.displayName }}
      <span v-if="session.user.isAdministrator" class="badge">Administrator</span>
    </span>
    <label class="sr-only" for="user-theme">Theme</label>
    <select
      id="user-theme"
      class="theme-select"
      title="Theme"
      :value="branding.userTheme ?? ''"
      @change="branding.setUserTheme((($event.target as HTMLSelectElement).value || null) as 'light' | 'dark' | 'system' | null)"
    >
      <option value="">Theme: default ({{ THEME_NAMES[branding.effective.defaultTheme] }})</option>
      <option value="light">Theme: light</option>
      <option value="dark">Theme: dark</option>
      <option value="system">Theme: follow system</option>
    </select>
    <button type="button" class="btn" :disabled="busy" @click="signOut">{{ busy ? "Signing out…" : "Sign out" }}</button>
    <div v-if="error" class="user-menu-error"><ErrorAlert :error="error" title="Sign-out failed" /></div>
  </div>
</template>
