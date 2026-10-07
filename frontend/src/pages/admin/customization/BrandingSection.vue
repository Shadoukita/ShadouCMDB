<script setup lang="ts">
import { computed, ref } from "vue";
import { useDeleteAsset, useUploadAsset, type AssetKind, type ImageType, type UiSettings, type UiSettingsDocument } from "../../../api/uiSettings";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import { contrast, HEX_COLOR } from "../../../lib/color";
import { assetUrl, DEFAULT_APP_NAME, useBrandingStore } from "../../../stores/branding";

/**
 * Customization › Branding. Name, colours and the default theme are part of the
 * settings document (saved with the rest); the logo and favicon are uploaded
 * and removed right away, because they are files, not settings.
 */
const props = defineProps<{ doc: UiSettingsDocument; assets: UiSettings["assets"] }>();
const b = computed(() => props.doc.branding);
const branding = useBrandingStore();

const COLORS = [
  { key: "primaryColor", label: "Primary colour", hint: "Buttons, links and focus rings", fallback: "#0b6e7f" },
  { key: "accentColor", label: "Accent colour", hint: "The marker of the current menu entry", fallback: "#1e95a8" },
] as const;
const colorText = ref<Record<string, string>>({ primaryColor: b.value.primaryColor ?? "", accentColor: b.value.accentColor ?? "" });
const colorError = ref<Record<string, string>>({});
function setColor(key: "primaryColor" | "accentColor", value: string) {
  colorText.value[key] = value;
  const v = value.trim();
  if (v === "") {
    b.value[key] = null;
    colorError.value[key] = "";
  } else if (HEX_COLOR.test(v)) {
    b.value[key] = v.toLowerCase();
    colorError.value[key] = "";
  } else {
    colorError.value[key] = "Use #rrggbb, e.g. #1f6feb";
  }
}
/** White button text on the primary colour: WCAG asks for 4.5:1 (the UI switches to dark text below that). */
const primaryContrast = computed(() => (b.value.primaryColor ? contrast(b.value.primaryColor, "#ffffff") : null));

// ---------- Logo and favicon ----------
const ASSETS: { kind: AssetKind; label: string; types: ImageType[]; max: number; hint: string }[] = [
  { kind: "logo", label: "Logo", types: ["image/png", "image/jpeg", "image/webp", "image/svg+xml"], max: 512 * 1024, hint: "PNG, JPEG, WebP or SVG up to 512 KiB. Shown in the header and on the sign-in page." },
  { kind: "favicon", label: "Favicon", types: ["image/png", "image/x-icon", "image/svg+xml"], max: 128 * 1024, hint: "PNG, ICO or SVG up to 128 KiB. Shown in the browser tab." },
];
const upload = useUploadAsset();
const remove = useDeleteAsset();
const assetError = ref<Record<string, unknown>>({});
const busyKind = ref<AssetKind | null>(null);
const confirmRemove = ref<AssetKind | null>(null);

function typeOf(file: File): string {
  if (file.type === "image/vnd.microsoft.icon" || /\.ico$/i.test(file.name)) return "image/x-icon";
  if (!file.type && /\.svg$/i.test(file.name)) return "image/svg+xml";
  return file.type;
}

async function onFile(kind: AssetKind, e: Event) {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = "";
  if (!file) return;
  const spec = ASSETS.find((a) => a.kind === kind)!;
  const type = typeOf(file);
  assetError.value[kind] = null;
  if (!spec.types.includes(type as ImageType)) {
    assetError.value[kind] = new Error(`${file.name} is ${type || "of an unknown type"}; the ${kind} must be ${spec.types.join(", ")}.`);
    return;
  }
  if (file.size > spec.max) {
    assetError.value[kind] = new Error(`${file.name} is ${Math.ceil(file.size / 1024)} KiB; the ${kind} can be at most ${spec.max / 1024} KiB.`);
    return;
  }
  busyKind.value = kind;
  try {
    const data = await readBase64(file);
    await upload.mutateAsync({ kind, contentType: type as ImageType, data });
    await branding.load();
  } catch (err) {
    assetError.value[kind] = err;
  } finally {
    busyKind.value = null;
  }
}

async function onRemove() {
  const kind = confirmRemove.value;
  if (!kind) return;
  busyKind.value = kind;
  try {
    await remove.mutateAsync(kind);
    await branding.load();
    confirmRemove.value = null;
  } catch (err) {
    assetError.value[kind] = err;
    confirmRemove.value = null;
  } finally {
    busyKind.value = null;
  }
}

function readBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).replace(/^data:[^,]*,/, ""));
    r.onerror = () => reject(r.error ?? new Error(`Could not read ${file.name}`));
    r.readAsDataURL(file);
  });
}
</script>

<template>
  <div class="editor-row">
    <section class="panel">
      <div class="panel-header"><h2>Name, colours and theme</h2></div>
      <div class="panel-body">
        <div class="form-grid">
          <div class="field wide">
            <label for="brand-name">Application name</label>
            <input
              id="brand-name"
              type="text"
              maxlength="60"
              :placeholder="DEFAULT_APP_NAME"
              :value="b.appName ?? ''"
              @input="b.appName = ($event.target as HTMLInputElement).value.trim() ? ($event.target as HTMLInputElement).value : null"
            />
            <span class="hint">Header, sign-in page and browser tab. Empty means “{{ DEFAULT_APP_NAME }}”.</span>
          </div>
          <div v-for="c in COLORS" :key="c.key" class="field">
            <label :for="`brand-${c.key}`">{{ c.label }}</label>
            <div class="inline-control">
              <input
                type="color"
                :aria-label="`${c.label} picker`"
                :value="b[c.key] ?? c.fallback"
                @input="setColor(c.key, ($event.target as HTMLInputElement).value)"
              />
              <input
                :id="`brand-${c.key}`"
                type="text"
                class="mono"
                placeholder="built-in"
                :value="colorText[c.key]"
                :aria-invalid="!!colorError[c.key] || undefined"
                :aria-describedby="`brand-${c.key}-hint`"
                @input="setColor(c.key, ($event.target as HTMLInputElement).value)"
              />
              <button type="button" class="btn btn-sm" :disabled="!b[c.key]" @click="setColor(c.key, '')">Default</button>
            </div>
            <span v-if="colorError[c.key]" class="error">{{ colorError[c.key] }}</span>
            <span :id="`brand-${c.key}-hint`" class="hint">{{ c.hint }}</span>
          </div>
          <div class="field wide">
            <span class="label" id="brand-theme-label">Default theme</span>
            <div class="inline-control" role="radiogroup" aria-labelledby="brand-theme-label">
              <label v-for="t in (['system', 'light', 'dark'] as const)" :key="t" class="check">
                <input v-model="b.defaultTheme" type="radio" name="brand-theme" :value="t" />
                {{ t === "system" ? "Follow the operating system" : t === "light" ? "Light" : "Dark" }}
              </label>
            </div>
            <span class="hint">For users who have not picked a theme in their user menu.</span>
          </div>
        </div>
        <p v-if="primaryContrast !== null && primaryContrast < 4.5" class="alert alert-warn">
          White text on this primary colour has a contrast of {{ primaryContrast.toFixed(1) }}:1, below the 4.5:1 that
          reads well; buttons use whichever of white or dark text reads better on it.
        </p>
      </div>
    </section>

    <section class="panel">
      <div class="panel-header"><h2>Preview</h2><span class="muted">The header and menu of this page show your changes too</span></div>
      <div class="panel-body">
        <div class="preview-frame" aria-label="Branding preview">
          <div class="brand-preview">
            <div class="brand-preview-side">
              <div class="brand-preview-name">
                <img v-if="assets.logo" :src="assetUrl(assets.logo.url)" alt="" class="brand-logo" />
                <span>{{ b.appName || DEFAULT_APP_NAME }}</span>
              </div>
              <div class="brand-preview-item active">Dashboard</div>
              <div class="brand-preview-item">All configuration items</div>
            </div>
            <div class="brand-preview-main">
              <button type="button" class="btn btn-primary" tabindex="-1">+ New CI</button>
              <a href="#" tabindex="-1" @click.prevent>A link to a related CI</a>
            </div>
          </div>
        </div>
      </div>
    </section>
  </div>

  <section class="panel" style="margin-top: var(--sp-4)">
    <div class="panel-header"><h2>Logo and favicon</h2><span class="muted">Uploaded and removed right away, not with Save</span></div>
    <div class="panel-body">
      <div class="form-grid">
        <div v-for="a in ASSETS" :key="a.kind" class="field">
          <span class="label">{{ a.label }}</span>
          <div class="inline-control">
            <div class="asset-preview">
              <img v-if="assets[a.kind]" :src="assetUrl(assets[a.kind]!.url)" :alt="`Current ${a.kind}`" />
              <span v-else class="muted">none</span>
            </div>
            <label class="btn btn-sm" :for="`asset-${a.kind}`">{{ busyKind === a.kind ? "Working…" : assets[a.kind] ? "Replace…" : "Upload…" }}</label>
            <input :id="`asset-${a.kind}`" class="sr-only" type="file" :accept="a.types.join(',') + (a.kind === 'favicon' ? ',.ico' : '')" :disabled="busyKind !== null" @change="onFile(a.kind, $event)" />
            <button v-if="assets[a.kind]" type="button" class="btn btn-sm" :disabled="busyKind !== null" @click="confirmRemove = a.kind">Remove</button>
          </div>
          <span class="hint">{{ a.hint }}</span>
          <span v-if="assets[a.kind]" class="hint">
            {{ assets[a.kind]!.contentType }}, {{ Math.ceil(assets[a.kind]!.size / 1024) }} KiB
          </span>
          <ErrorAlert v-if="assetError[a.kind]" :error="assetError[a.kind]" :title="`${a.label} not saved`" />
        </div>
      </div>
    </div>
  </section>

  <ConfirmDialog :open="confirmRemove !== null" :title="`Remove the ${confirmRemove}?`" confirm-label="Remove" :busy="busyKind !== null" @confirm="onRemove" @cancel="confirmRemove = null">
    <template v-if="confirmRemove === 'logo'">The header and sign-in page show only the application name again.</template>
    <template v-else>Browser tabs show no icon again.</template>
    The change applies to every user immediately and is recorded in the audit log.
  </ConfirmDialog>
</template>

<style scoped>
.brand-preview {
  display: grid;
  grid-template-columns: 180px 1fr;
  min-height: 120px;
  border: 1px solid var(--c-border);
  border-radius: var(--radius);
  overflow: hidden;
}
.brand-preview-side {
  background: var(--c-sidebar);
  color: var(--c-sidebar-text);
  padding: var(--sp-3) 0;
}
.brand-preview-name {
  display: flex;
  align-items: center;
  gap: var(--sp-3);
  color: var(--c-sidebar-text-strong);
  font-weight: var(--fw-semibold);
  padding: 0 var(--sp-4) var(--sp-3);
}
.brand-preview-item {
  padding: 4px var(--sp-4);
  font-size: var(--fs-md);
}
.brand-preview-item.active {
  background: var(--c-sidebar-active);
  color: var(--c-sidebar-text-strong);
  box-shadow: inset 3px 0 0 var(--c-accent);
}
.brand-preview-main {
  display: flex;
  align-items: flex-start;
  gap: var(--sp-4);
  padding: var(--sp-4);
  background: var(--c-surface);
}
</style>
