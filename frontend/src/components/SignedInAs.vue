<script setup lang="ts">
import { computed } from "vue";
import { t } from "../i18n";

/** "Signed in as {name} ({username})." in the foot of a stand-alone card, the name in bold and the username in mono. */
defineProps<{ displayName: string; username: string }>();

/** The sentence split around both slots, so each locale keeps its own word order. */
const parts = computed(() => {
  const [before = "", rest = ""] = t("account.signedInAs", { name: "\u0000", username: "\u0001" }).split("\u0000");
  const [middle = "", after = ""] = rest.split("\u0001");
  return [before, middle, after];
});
</script>

<template>
  <p class="hint">{{ parts[0] }}<strong>{{ displayName }}</strong>{{ parts[1] }}<code>{{ username }}</code>{{ parts[2] }}</p>
</template>
