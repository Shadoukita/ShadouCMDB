<script setup lang="ts">
import type { IconName } from "../icons/lucide";
import Icon from "./Icon.vue";

/**
 * An empty list, a query without results or a missing permission, in place of the content it
 * replaces: an icon, a title, a sentence and the action that gets the operator going.
 */
withDefaults(defineProps<{ title?: string; icon?: IconName | null }>(), { title: undefined, icon: "inbox" });
</script>

<template>
  <div class="state">
    <Icon v-if="icon" :name="icon" :size="32" class="state-icon" />
    <h2 v-if="title">{{ title }}</h2>
    <div v-if="$slots.default"><slot /></div>
    <div v-if="$slots.actions" class="actions"><slot name="actions" /></div>
  </div>
</template>
