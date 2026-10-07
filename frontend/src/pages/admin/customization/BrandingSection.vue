<script setup lang="ts">
import { computed, ref } from "vue";
import { useDeleteAsset, useUploadAsset, type AssetKind, type ImageType, type UiSettings, type UiSettingsDocument } from "../../../api/uiSettings";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import BrandMark from "../../../components/BrandMark.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import Icon from "../../../components/Icon.vue";
import { t } from "../../../i18n";
import { brandVariables, DEFAULT_ACCENT, DEFAULT_PRIMARY, type Theme } from "../../../lib/brandColors";
import { contrast, HEX_COLOR } from "../../../lib/color";
import { assetUrl, DEFAULT_APP_NAME, useBrandingStore } from "../../../stores/branding";

/**
 * Customization › Branding. Name, colours and the default theme are part of the
 * settings document (saved with the rest); the logo and favicon are uploaded
 * and removed right away, because they are files, not settings. The preview draws the
 * rail and the sign-in card, where the logo appears, in both themes (audit A9).
 */
const props = defineProps<{ doc: UiSettingsDocument; assets: UiSettings["assets"] }>();
const b = computed(() => props.doc.branding);
const branding = useBrandingStore();

const COLORS = [
  { key: "primaryColor", label: t("cust.branding.primary"), hint: t("cust.branding.primaryHint"), fallback: DEFAULT_PRIMARY },
  { key: "accentColor", label: t("cust.branding.accent"), hint: t("cust.branding.accentHint"), fallback: DEFAULT_ACCENT },
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
    colorError.value[key] = t("cust.branding.colorFormat");
  }
}
/** White button text on the primary colour: WCAG asks for 4.5:1 (the UI switches to dark text below that). */
const primaryContrast = computed(() => (b.value.primaryColor ? contrast(b.value.primaryColor, "#ffffff") : null));

// ---------- Logo and favicon ----------
const ASSETS: { kind: AssetKind; label: string; types: ImageType[]; max: number; hint: string }[] = [
  { kind: "logo", label: t("cust.branding.logo"), types: ["image/png", "image/jpeg", "image/webp", "image/svg+xml"], max: 512 * 1024, hint: t("cust.branding.logoHint") },
  { kind: "favicon", label: t("cust.branding.favicon"), types: ["image/png", "image/x-icon", "image/svg+xml"], max: 128 * 1024, hint: t("cust.branding.faviconHint") },
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
    assetError.value[kind] = new Error(t("cust.branding.wrongType", { file: file.name, type: type || "?", label: spec.label, types: spec.types.join(", ") }));
    return;
  }
  if (file.size > spec.max) {
    assetError.value[kind] = new Error(t("cust.branding.tooLarge", { file: file.name, size: Math.ceil(file.size / 1024), label: spec.label, max: spec.max / 1024 }));
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
    r.onerror = () => reject(r.error ?? new Error(t("cust.branding.unreadable", { file: file.name })));
    r.readAsDataURL(file);
  });
}

// ---------- Preview: the rail and the sign-in card, in each theme ----------
const THEMES: Theme[] = ["light", "dark"];
/** The draft's brand colours for one theme, set on that preview only (tokens.css re-declares the defaults there). */
const previewStyle = (theme: Theme) => brandVariables(b.value.primaryColor, b.value.accentColor, theme);
const pickerValue = (c: (typeof COLORS)[number]) => b.value[c.key] ?? c.fallback[branding.theme];
const assetLabel = (kind: AssetKind | null) => ASSETS.find((a) => a.kind === kind)?.label ?? "";
</script>

<template>
  <div class="editor-row">
    <section class="panel" aria-labelledby="brand-settings-title">
      <div class="panel-header"><h2 id="brand-settings-title">{{ t("cust.branding.settingsTitle") }}</h2></div>
      <div class="panel-body">
        <div class="form-grid">
          <div class="field wide">
            <label for="brand-name">{{ t("cust.branding.appName") }}</label>
            <input
              id="brand-name"
              type="text"
              maxlength="60"
              :placeholder="DEFAULT_APP_NAME"
              :value="b.appName ?? ''"
              aria-describedby="brand-name-hint"
              @input="b.appName = ($event.target as HTMLInputElement).value.trim() ? ($event.target as HTMLInputElement).value : null"
            />
            <span id="brand-name-hint" class="hint">{{ t("cust.branding.appNameHint", { name: DEFAULT_APP_NAME }) }}</span>
          </div>
          <div v-for="c in COLORS" :key="c.key" class="field">
            <label :for="`brand-${c.key}`">{{ c.label }}</label>
            <div class="inline-control">
              <input type="color" :aria-label="t('cust.branding.picker', { label: c.label })" :value="pickerValue(c)" @input="setColor(c.key, ($event.target as HTMLInputElement).value)" />
              <input
                :id="`brand-${c.key}`"
                type="text"
                class="mono"
                :placeholder="t('cust.branding.builtIn')"
                :value="colorText[c.key]"
                :aria-invalid="!!colorError[c.key] || undefined"
                :aria-describedby="`brand-${c.key}-hint`"
                @input="setColor(c.key, ($event.target as HTMLInputElement).value)"
              />
              <button type="button" class="btn btn-sm" :disabled="!b[c.key]" @click="setColor(c.key, '')">{{ t("cust.branding.default") }}</button>
            </div>
            <span v-if="colorError[c.key]" class="error">{{ colorError[c.key] }}</span>
            <span :id="`brand-${c.key}-hint`" class="hint">{{ c.hint }}</span>
          </div>
          <fieldset class="field wide">
            <legend class="label">{{ t("cust.branding.defaultTheme") }}</legend>
            <div class="inline-control">
              <label v-for="th in (['system', 'light', 'dark'] as const)" :key="th" class="check">
                <input v-model="b.defaultTheme" type="radio" name="brand-theme" :value="th" aria-describedby="brand-theme-hint" />
                {{ th === "system" ? t("cust.branding.themeSystem") : th === "light" ? t("cust.branding.themeLight") : t("cust.branding.themeDark") }}
              </label>
            </div>
            <span id="brand-theme-hint" class="hint">{{ t("cust.branding.defaultThemeHint") }}</span>
          </fieldset>
        </div>
        <p v-if="primaryContrast !== null && primaryContrast < 4.5" class="alert alert-warn">
          {{ t("cust.branding.lowContrast", { ratio: primaryContrast.toFixed(1) }) }}
        </p>
      </div>
    </section>

    <section class="panel" aria-labelledby="brand-preview-title">
      <div class="panel-header"><h2 id="brand-preview-title">{{ t("cust.branding.preview") }}</h2><span class="muted">{{ t("cust.branding.previewNote") }}</span></div>
      <div class="panel-body brand-previews">
        <figure v-for="th in THEMES" :key="th" class="brand-preview" :data-theme-scope="th" :style="previewStyle(th)" :data-testid="`brand-preview-${th}`">
          <figcaption>{{ th === "light" ? t("cust.branding.themeLight") : t("cust.branding.themeDark") }}</figcaption>
          <div class="brand-preview-body" aria-hidden="true">
            <div class="brand-preview-rail">
              <div class="brand-preview-mark"><BrandMark /></div>
              <div class="brand-preview-item active"><Icon name="layout-dashboard" />{{ t("nav.page.dashboard") }}</div>
              <div class="brand-preview-item"><Icon name="list" />{{ t("cust.branding.previewInventory") }}</div>
            </div>
            <div class="brand-preview-page">
              <div class="brand-preview-card">
                <div class="brand-preview-card-mark"><BrandMark /></div>
                <strong>{{ t("auth.signIn.title") }}</strong>
                <span class="brand-preview-input" />
                <span class="btn btn-primary btn-sm">{{ t("auth.signIn.submit") }}</span>
              </div>
              <span class="brand-preview-link">{{ t("cust.branding.previewLink") }}</span>
            </div>
          </div>
        </figure>
      </div>
    </section>
  </div>

  <section class="panel brand-assets" aria-labelledby="brand-assets-title">
    <div class="panel-header"><h2 id="brand-assets-title">{{ t("cust.branding.assetsTitle") }}</h2><span class="muted">{{ t("cust.branding.assetsNote") }}</span></div>
    <div class="panel-body">
      <div class="form-grid">
        <div v-for="a in ASSETS" :key="a.kind" class="field">
          <span class="label">{{ a.label }}</span>
          <div class="inline-control">
            <div class="asset-preview">
              <img v-if="assets[a.kind]" :src="assetUrl(assets[a.kind]!.url)" :alt="t('cust.branding.current', { label: a.label })" />
              <span v-else class="muted">{{ t("cust.branding.none") }}</span>
            </div>
            <label class="btn btn-sm" :for="`asset-${a.kind}`"><Icon name="upload" />{{ busyKind === a.kind ? t("common.working") : assets[a.kind] ? t("cust.branding.replace") : t("cust.branding.upload") }}</label>
            <input :id="`asset-${a.kind}`" class="sr-only" type="file" :accept="a.types.join(',') + (a.kind === 'favicon' ? ',.ico' : '')" :disabled="busyKind !== null" @change="onFile(a.kind, $event)" />
            <button v-if="assets[a.kind]" type="button" class="btn btn-sm btn-quiet-danger" :disabled="busyKind !== null" @click="confirmRemove = a.kind">{{ t("cust.branding.remove") }}</button>
          </div>
          <span class="hint">{{ a.hint }}</span>
          <span v-if="assets[a.kind]" class="hint">{{ t("cust.branding.assetSize", { type: assets[a.kind]!.contentType, size: Math.ceil(assets[a.kind]!.size / 1024) }) }}</span>
          <ErrorAlert v-if="assetError[a.kind]" :error="assetError[a.kind]" :title="t('cust.branding.assetNotSaved', { label: a.label })" />
        </div>
      </div>
    </div>
  </section>

  <ConfirmDialog
    :open="confirmRemove !== null"
    :title="t('cust.branding.removeTitle', { label: assetLabel(confirmRemove) })"
    :confirm-label="t('cust.branding.remove')"
    :busy="busyKind !== null"
    @confirm="onRemove"
    @cancel="confirmRemove = null"
  >
    {{ confirmRemove === "logo" ? t("cust.branding.removeLogoBody") : t("cust.branding.removeFaviconBody") }}
    {{ t("cust.branding.removeAudited") }}
  </ConfirmDialog>
</template>
