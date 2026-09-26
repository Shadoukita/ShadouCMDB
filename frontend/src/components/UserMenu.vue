<script setup lang="ts">
import { ref } from "vue";
import { useRouter } from "vue-router";
import { useSessionStore } from "../stores/session";
import ErrorAlert from "./ErrorAlert.vue";

/** The acting user and "Sign out" in the header. */
const session = useSessionStore();
const router = useRouter();
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
    <button type="button" class="btn" :disabled="busy" @click="signOut">{{ busy ? "Signing out…" : "Sign out" }}</button>
    <div v-if="error" class="user-menu-error"><ErrorAlert :error="error" title="Sign-out failed" /></div>
  </div>
</template>
