<script setup lang="ts">
import type { LayoutEditor } from "../../lib/layoutEditor";

/**
 * "Edit layout" on a CI page: opens the class's layout editor in its own window.
 * Where the browser cannot tell this window that the editor saved (no
 * BroadcastChannel), it offers a reload instead.
 */
defineProps<{ editor: LayoutEditor }>();
const reload = () => window.location.reload();
</script>

<template>
  <button type="button" class="btn" title="Opens the layout editor in a new window" @click="editor.enter()">Edit layout</button>
  <span v-if="editor.reloadHint" class="muted" role="status">
    Reload this page after saving the layout to see it.
    <button type="button" class="btn btn-sm" @click="reload">Reload</button>
  </span>
</template>
