<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { useRouter } from "vue-router";
import { useCloneProfile, type PermissionProfile } from "../../api/admin";
import { ApiError } from "../../api/client";
import ErrorAlert from "../../components/ErrorAlert.vue";
import { t } from "../../i18n";
import { useFlashStore } from "../../stores/flash";

/** Asks for the copy's name, clones through the API, then opens the copy for editing. */
const props = defineProps<{ profile: PermissionProfile | null }>();
const emit = defineEmits<{ close: [] }>();
const router = useRouter();
const flash = useFlashStore();
const clone = useCloneProfile();
const dialog = ref<HTMLDialogElement>();
const input = ref<HTMLInputElement>();
const name = ref("");
const nameError = computed(() => (clone.error.value instanceof ApiError ? clone.error.value.fieldErrors().name : undefined));

watch(
  () => props.profile,
  async (p) => {
    const d = dialog.value;
    if (!d) return;
    if (p) {
      clone.reset();
      name.value = t("admin.clone.defaultName", { name: p.name });
      if (!d.open) d.showModal();
      await nextTick();
      input.value?.select();
    } else if (d.open) d.close();
  },
);

function cancel(e?: Event) {
  e?.preventDefault();
  if (!clone.isPending.value) emit("close");
}

function submit() {
  if (!props.profile || !name.value.trim()) return;
  clone.mutate(
    { id: props.profile.id, name: name.value.trim() },
    {
      onSuccess: (copy) => {
        emit("close");
        if (copy) {
          flash.show(t("admin.clone.created", { name: copy.name, source: props.profile?.name ?? "" }));
          router.push(`/admin/profiles/${copy.id}`);
        }
      },
    },
  );
}
</script>

<template>
  <dialog ref="dialog" class="confirm" aria-labelledby="clone-title" @cancel="cancel">
    <form @submit.prevent="submit">
      <h2 id="clone-title">{{ t("admin.clone.title", { name: profile?.name ?? "" }) }}</h2>
      <div class="body stack">
        <p class="no-margin">{{ t("admin.clone.body") }}</p>
        <ErrorAlert v-if="clone.isError.value && !nameError" :error="clone.error.value" :title="t('admin.clone.failed')" />
        <div class="field">
          <label for="clone-name">{{ t("admin.clone.name") }}<span class="req" aria-hidden="true">*</span></label>
          <input
            id="clone-name"
            ref="input"
            v-model="name"
            type="text"
            required
            :aria-invalid="!!nameError"
            :aria-describedby="nameError ? 'clone-name-err' : undefined"
          />
          <span v-if="nameError" id="clone-name-err" class="error">{{ nameError }}</span>
        </div>
      </div>
      <div class="footer">
        <button type="button" class="btn" :disabled="clone.isPending.value" @click="cancel()">{{ t("common.cancel") }}</button>
        <button type="submit" class="btn btn-primary" :disabled="clone.isPending.value || !name.trim()">
          {{ clone.isPending.value ? t("admin.clone.busy") : t("admin.clone.submit") }}
        </button>
      </div>
    </form>
  </dialog>
</template>
