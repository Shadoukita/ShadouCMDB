<script setup lang="ts">
import { ref } from "vue";

/**
 * Freshly issued recovery codes, shown once. They live only in the parent's
 * state until the user confirms they saved them; nothing is cached or stored.
 */
const props = defineProps<{ codes: string[]; username: string }>();
const emit = defineEmits<{ done: [] }>();
const saved = ref(false);
const copyState = ref<"" | "copied" | "failed">("");
const list = ref<HTMLElement>();

const asText = () =>
  [`ShadouCMDB recovery codes for ${props.username}`, `Created ${new Date().toISOString()}`, "Each code signs in once.", "", ...props.codes, ""].join("\n");

async function copy() {
  try {
    await navigator.clipboard.writeText(props.codes.join("\n"));
    copyState.value = "copied";
  } catch {
    // No Clipboard API (plain http) or permission refused: fall back to copying the selected text.
    const range = document.createRange();
    if (list.value) range.selectNodeContents(list.value);
    const sel = window.getSelection();
    sel?.removeAllRanges();
    sel?.addRange(range);
    copyState.value = document.execCommand("copy") ? "copied" : "failed";
  }
}

function download() {
  const url = URL.createObjectURL(new Blob([asText()], { type: "text/plain" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = `shadoucmdb-recovery-codes-${props.username}.txt`;
  a.click();
  URL.revokeObjectURL(url);
}
</script>

<template>
  <section class="stack" aria-labelledby="recovery-title">
    <h3 id="recovery-title" class="recovery-title">Your recovery codes</h3>
    <div class="alert alert-warn" role="status">
      <strong>Save these codes now. You won't see them again.</strong> Each one signs you in once if you lose your
      authenticator. Keep them in a password manager or print them, apart from your device.
    </div>
    <ol ref="list" class="recovery-codes mono" aria-label="Recovery codes">
      <li v-for="c in codes" :key="c">{{ c }}</li>
    </ol>
    <div class="actions">
      <button type="button" class="btn" @click="copy">Copy</button>
      <button type="button" class="btn" @click="download">Download .txt</button>
      <span role="status" :class="copyState === 'failed' ? 'error' : 'muted'">
        {{ copyState === "copied" ? "Copied to the clipboard." : copyState === "failed" ? "Could not copy — select the codes and copy them by hand." : "" }}
      </span>
    </div>
    <label class="checkbox-row"><input v-model="saved" type="checkbox" /> I have saved these recovery codes</label>
    <div><button type="button" class="btn btn-primary" :disabled="!saved" @click="emit('done')">Done</button></div>
  </section>
</template>

<style scoped>
.recovery-title {
  margin: 0;
  font-size: var(--fs-md);
}
.recovery-codes {
  display: grid;
  grid-template-columns: repeat(2, max-content);
  gap: var(--sp-2) var(--sp-6);
  margin: 0;
  padding: var(--sp-3) var(--sp-4);
  list-style: none;
  background: var(--c-surface-alt);
  border: 1px solid var(--c-border);
  border-radius: var(--radius);
  font-size: var(--fs-md);
}
</style>
