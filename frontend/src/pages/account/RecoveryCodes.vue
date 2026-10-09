<script setup lang="ts">
import { ref } from "vue";
import { t } from "../../i18n";

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
  [
    t("account.recovery.fileTitle", { username: props.username }),
    t("account.recovery.fileCreated", { date: new Date().toISOString() }),
    t("account.recovery.fileNote"),
    "",
    ...props.codes,
    "",
  ].join("\n");

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
    <h3 id="recovery-title" class="recovery-title">{{ t("account.recovery.title") }}</h3>
    <div class="alert alert-warn" role="status">
      <strong>{{ t("account.recovery.saveNow") }}</strong> {{ t("account.recovery.saveNowBody") }}
    </div>
    <ol ref="list" class="recovery-codes mono" :aria-label="t('account.recovery.list')">
      <li v-for="c in codes" :key="c">{{ c }}</li>
    </ol>
    <div class="actions">
      <button type="button" class="btn" @click="copy">{{ t("account.recovery.copy") }}</button>
      <button type="button" class="btn" @click="download">{{ t("account.recovery.download") }}</button>
      <span role="status" :class="['copy-status', copyState]" data-testid="recovery-copy-status">
        {{ copyState === "copied" ? t("account.recovery.copied") : copyState === "failed" ? t("account.recovery.copyFailed") : "" }}
      </span>
    </div>
    <label class="checkbox-row"><input v-model="saved" type="checkbox" /> {{ t("account.recovery.saved") }}</label>
    <div><button type="button" class="btn btn-primary" :disabled="!saved" @click="emit('done')">{{ t("account.recovery.done") }}</button></div>
  </section>
</template>

<style scoped>
/* The section's title (U2): the panel title's size, so it reads as the next step and not as body text. */
.recovery-title {
  margin: 0;
  font-size: var(--fs-h2);
  font-weight: var(--fw-semibold);
  line-height: 20px;
}
.copy-status {
  display: inline-flex;
  align-items: flex-start;
  gap: var(--space-1);
  font-size: var(--fs-sm);
  line-height: 16px;
  color: var(--c-text-secondary);
}
.copy-status.copied {
  color: var(--c-success-text);
}
/* Copying failed: the field-error style, with its icon, since the user has to act. */
.copy-status.failed {
  color: var(--c-danger-text);
}
.copy-status.failed::before {
  content: "";
  flex: none;
  width: 14px;
  height: 14px;
  margin-top: 1px;
  background: currentColor;
  mask: var(--icon-circle-alert) center / contain no-repeat;
}
.recovery-codes {
  display: grid;
  grid-template-columns: repeat(2, max-content);
  gap: var(--space-1) var(--space-6);
  margin: 0;
  padding: var(--space-2) var(--space-3);
  list-style: none;
  background: var(--c-surface-alt);
  border: 1px solid var(--c-border);
  border-radius: var(--radius-sm);
  font-size: var(--fs-md);
}
</style>
