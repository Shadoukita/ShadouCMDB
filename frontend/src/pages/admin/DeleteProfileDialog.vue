<script setup lang="ts">
import { computed } from "vue";
import { useDeleteProfile, useUserList, type PermissionProfile } from "../../api/admin";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { t } from "../../i18n";
import { useSessionStore } from "../../stores/session";

/**
 * Delete a profile, with a confirmation naming the users who lose it (when the viewer may list users).
 * Opened from the profile page's `⋯` menu; emits `deleted` once the profile is gone.
 */
const props = defineProps<{ profile: PermissionProfile }>();
const open = defineModel<boolean>("open", { required: true });
const emit = defineEmits<{ deleted: [] }>();
const session = useSessionStore();
const del = useDeleteProfile();
const LIST = 50;
const mayListUsers = computed(() => session.can("users.manage") && props.profile.userCount > 0);
const holders = useUserList(
  () => ({ profileId: props.profile.id, limit: LIST, sort: "username" as const }),
  () => open.value && mayListUsers.value,
);
const names = computed(() => (mayListUsers.value ? (holders.data.value?.data ?? []) : []));

function cancel() {
  del.reset();
  open.value = false;
}

function confirm() {
  del.mutate(props.profile.id, { onSuccess: () => emit("deleted") });
}
</script>

<template>
  <ConfirmDialog
    :open="open"
    :title="t('admin.profile.delete.title', { name: profile.name })"
    :confirm-label="profile.userCount > 0 ? t('admin.profile.delete.confirmUsers', { n: profile.userCount }) : t('admin.profile.delete.confirm')"
    :busy="del.isPending.value"
    @cancel="cancel"
    @confirm="confirm"
  >
    <ErrorAlert v-if="del.isError.value" :error="del.error.value" :title="t('groups.delete.failed')" />
    <p v-if="profile.userCount === 0">{{ t("admin.profile.delete.noUsers") }}</p>
    <template v-else>
      <p>{{ t("admin.profile.delete.users", { n: profile.userCount }) }}</p>
      <LoadingState v-if="mayListUsers && holders.isLoading.value" :label="t('admin.users.loading')" />
      <ul v-if="names.length > 0">
        <li v-for="u in names" :key="u.id"><span dir="auto">{{ u.displayName }}</span> <span class="muted">({{ u.username }})</span></li>
      </ul>
      <p v-if="names.length > 0 && profile.userCount > names.length" class="muted">{{ t("admin.profile.delete.more", { n: profile.userCount - names.length }) }}</p>
    </template>
  </ConfirmDialog>
</template>
