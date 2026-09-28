<script setup lang="ts">
import type { Inline } from "../lib/noteMarkdown";

/** One line of a note: text, emphasis, code and links (safe schemes only, see lib/noteMarkdown). */
defineProps<{ parts: Inline[] }>();
</script>

<template>
  <template v-for="(p, i) in parts" :key="i">
    <strong v-if="p.t === 'strong'">{{ p.text }}</strong>
    <em v-else-if="p.t === 'em'">{{ p.text }}</em>
    <code v-else-if="p.t === 'code'">{{ p.text }}</code>
    <a v-else-if="p.t === 'link'" :href="p.href" target="_blank" rel="noopener noreferrer">{{ p.text }}</a>
    <template v-else>{{ p.text }}</template>
  </template>
</template>
