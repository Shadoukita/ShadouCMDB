<script setup lang="ts">
defineProps<{ title: string }>();
</script>

<template>
  <div class="state">
    <h2>{{ title }}</h2>
    <div v-if="$slots.default"><slot /></div>
    <div v-if="$slots.actions" class="actions"><slot name="actions" /></div>
  </div>
</template>
