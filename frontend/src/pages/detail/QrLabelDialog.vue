<script setup lang="ts">
import { computed, onMounted, ref, useId, watch } from "vue";
import { useRouter } from "vue-router";
import type { Ci } from "../../api/queries";
import QrCode from "../../components/QrCode.vue";
import { t } from "../../i18n";
import { qrFileName, qrLabelPng, qrLabelSvg, saveBlob, type QrLabel } from "../../lib/qr";

/**
 * The CI's QR label (gap G12): a QR code of the CI's permalink with its name and ident under it, to print or
 * download as SVG or PNG. Everything is drawn in the browser (lib/qr) and nothing is sent anywhere, so it works on
 * an install without internet access. The permalink is this app's own address for the CI, whatever host and
 * base path it is served under.
 */
const props = defineProps<{ open: boolean; ci: Ci }>();
const emit = defineEmits<{ "update:open": [boolean] }>();
const router = useRouter();
const dialog = ref<HTMLDialogElement>();
const titleId = `qr-title-${useId()}`;
const error = ref("");

const permalink = computed(() => new URL(router.resolve(`/cis/${props.ci.id}`).href, window.location.origin).href);
const label = computed<QrLabel>(() => ({ value: permalink.value, title: props.ci.label, subtitle: props.ci.ident }));

function sync() {
  const d = dialog.value;
  if (!d) return;
  if (props.open && !d.open) d.showModal();
  if (!props.open && d.open) d.close();
}
onMounted(sync);
watch(() => props.open, sync);
watch(
  () => props.open,
  () => (error.value = ""),
);
const close = () => emit("update:open", false);

function downloadSvg() {
  saveBlob(new Blob([qrLabelSvg(label.value)], { type: "image/svg+xml" }), qrFileName(props.ci.ident, "svg"));
}
async function downloadPng() {
  error.value = "";
  try {
    saveBlob(await qrLabelPng(label.value), qrFileName(props.ci.ident, "png"));
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  }
}
/** Prints the label only (the print stylesheet hides the rest of the page while the dialog is open). */
function print() {
  document.documentElement.classList.add("printing-qr");
  const done = () => {
    document.documentElement.classList.remove("printing-qr");
    window.removeEventListener("afterprint", done);
  };
  window.addEventListener("afterprint", done);
  window.print();
}
</script>

<template>
  <Teleport to="body">
    <dialog ref="dialog" class="confirm qr-dialog" :aria-labelledby="titleId" data-testid="qr-dialog" @cancel.prevent="close">
      <h2 :id="titleId">{{ t("record.qr.title") }}</h2>
      <div class="body">
        <figure v-if="open" class="qr-label" data-testid="qr-label">
          <QrCode :value="permalink" :label="t('record.qr.alt', { name: ci.label })" :size="224" />
          <figcaption>
            <strong dir="auto">{{ ci.label }}</strong>
            <span class="mono">{{ ci.ident }}</span>
          </figcaption>
        </figure>
        <p class="hint qr-hint">{{ t("record.qr.hint") }}</p>
        <div class="field">
          <span class="label">{{ t("record.qr.link") }}</span>
          <a class="mono qr-link" :href="permalink" data-testid="qr-permalink">{{ permalink }}</a>
        </div>
        <div v-if="error" class="alert alert-error" role="alert">{{ error }}</div>
      </div>
      <div class="footer">
        <button type="button" class="btn" @click="downloadSvg">{{ t("record.qr.svg") }}</button>
        <button type="button" class="btn" @click="downloadPng">{{ t("record.qr.png") }}</button>
        <button type="button" class="btn" @click="print">{{ t("record.qr.print") }}</button>
        <button type="button" class="btn btn-primary" autofocus @click="close">{{ t("record.qr.close") }}</button>
      </div>
    </dialog>
  </Teleport>
</template>

<style scoped>
.qr-dialog {
  width: min(28rem, 92vw);
}
.qr-label {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--space-2);
  margin: 0 0 var(--space-3);
}
.qr-label figcaption {
  display: flex;
  flex-direction: column;
  align-items: center;
  max-width: 224px;
  text-align: center;
  overflow-wrap: anywhere;
}
.qr-hint {
  margin: 0 0 var(--space-3);
}
.qr-link {
  overflow-wrap: anywhere;
  font-size: var(--fs-sm);
}
.footer .btn-primary {
  margin-left: auto;
}
</style>

<style>
/* Print: the label alone, on a white page. */
@media print {
  html.printing-qr body > :not(dialog.qr-dialog) {
    display: none !important;
  }
  html.printing-qr dialog.qr-dialog {
    position: static;
    border: 0;
    box-shadow: none;
    margin: 0 auto;
    padding: 0;
  }
  html.printing-qr dialog.qr-dialog::backdrop {
    background: none;
  }
  html.printing-qr dialog.qr-dialog > :not(.body),
  html.printing-qr dialog.qr-dialog .body > :not(.qr-label) {
    display: none !important;
  }
  html.printing-qr .qr-label .qr {
    border: 0;
  }
  html.printing-qr .qr-label figcaption {
    color: #000;
  }
}
</style>
