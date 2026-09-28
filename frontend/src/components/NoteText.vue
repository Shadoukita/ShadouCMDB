<script setup lang="ts">
import { computed } from "vue";
import { parseNote } from "../lib/noteMarkdown";
import NoteInline from "./NoteInline.vue";

/** A layout note's text as limited Markdown (lib/noteMarkdown), built from elements and text nodes only: no raw HTML. */
const props = defineProps<{ text: string }>();
const blocks = computed(() => parseNote(props.text));
</script>

<template>
  <div class="note-text">
    <template v-for="(b, i) in blocks" :key="i">
      <p v-if="b.t === 'p'">
        <template v-for="(line, j) in b.lines" :key="j"><br v-if="j > 0" /><NoteInline :parts="line" /></template>
      </p>
      <component :is="b.t" v-else>
        <li v-for="(item, j) in b.items" :key="j"><NoteInline :parts="item" /></li>
      </component>
    </template>
  </div>
</template>

<style scoped>
.note-text {
  white-space: normal;
  overflow-wrap: anywhere;
}
.note-text > :first-child {
  margin-top: 0;
}
.note-text > :last-child {
  margin-bottom: 0;
}
.note-text p,
.note-text ul,
.note-text ol {
  margin: 0 0 var(--sp-2);
}
.note-text ul,
.note-text ol {
  padding-left: var(--sp-5);
}
</style>
