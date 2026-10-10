<script setup lang="ts">
import { nextTick, onMounted, ref } from "vue";
import Icon from "../../../components/Icon.vue";
import { t } from "../../../i18n";

/**
 * A signing secret, shown once (SHAA-2725 §5.4): in the data font, selected, with a copy button and how the
 * receiver uses it. The parent holds the only copy and drops it when its dialog closes.
 */
const props = defineProps<{ secret: string; graceUntil?: string | null }>();
const input = ref<HTMLInputElement>();
const copyState = ref<"" | "copied" | "failed">("");

onMounted(async () => {
  await nextTick();
  input.value?.focus();
  input.value?.select();
});

async function copy() {
  try {
    await navigator.clipboard.writeText(props.secret);
    copyState.value = "copied";
  } catch {
    // No Clipboard API (plain http) or permission refused: fall back to copying the selected text.
    input.value?.select();
    copyState.value = document.execCommand("copy") ? "copied" : "failed";
  }
}
</script>

<template>
  <div class="stack">
    <div class="alert alert-warn" role="alert">
      <strong>{{ t("webhooks.secret.copyNow") }}</strong>
      <div>{{ t("webhooks.secret.copyNowBody") }}</div>
    </div>
    <div class="field">
      <label for="webhook-secret">{{ t("webhooks.secret.label") }}</label>
      <div class="token-copy-row">
        <input
          id="webhook-secret"
          ref="input"
          class="mono"
          type="text"
          readonly
          spellcheck="false"
          autocomplete="off"
          data-testid="webhook-secret"
          :value="secret"
          @focus="input?.select()"
        />
        <button type="button" class="btn" @click="copy">
          <Icon :name="copyState === 'copied' ? 'check' : 'copy'" :size="16" />{{ t("admin.token.copy") }}
        </button>
      </div>
      <span class="hint">{{ t("webhooks.secret.howTo") }} <code>X-ShadouCMDB-Signature: t=…,v1=…</code></span>
      <span v-if="graceUntil" class="hint">{{ graceUntil }}</span>
      <span role="status" :class="copyState === 'failed' ? 'error' : 'hint'">
        {{ copyState === "copied" ? t("admin.token.copied") : copyState === "failed" ? t("admin.token.copyFailed") : "" }}
      </span>
    </div>
  </div>
</template>
