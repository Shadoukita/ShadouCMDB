<script setup lang="ts">
import { computed, ref } from "vue";
import { useRouter } from "vue-router";
import { useDeleteProfile, useUserList, type PermissionProfile } from "../../api/admin";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import { plural } from "../../lib/format";
import { useSessionStore } from "../../stores/session";

/** Delete with a confirmation naming the users who lose the profile (when the viewer may list users). */
const props = defineProps<{ profile: PermissionProfile }>();
const router = useRouter();
const session = useSessionStore();
const open = ref(false);
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
  del.mutate(props.profile.id, { onSuccess: () => router.replace("/admin/profiles") });
}
</script>

<template>
  <button type="button" class="btn btn-danger" @click="open = true">Delete</button>
  <ConfirmDialog
    :open="open"
    :title="`Delete profile “${profile.name}”?`"
    :confirm-label="profile.userCount > 0 ? `Delete and remove from ${plural(profile.userCount, 'user')}` : 'Delete profile'"
    :busy="del.isPending.value"
    @cancel="cancel"
    @confirm="confirm"
  >
    <ErrorAlert v-if="del.isError.value" :error="del.error.value" title="Delete failed" />
    <p v-if="profile.userCount === 0">No user holds this profile, so nobody loses access.</p>
    <template v-else>
      <p>
        <strong>{{ plural(profile.userCount, "user") }}</strong> hold this profile and lose the permissions it grants (unless
        another of their profiles grants them too):
      </p>
      <LoadingState v-if="mayListUsers && holders.isLoading.value" label="Loading users…" />
      <ul v-if="names.length > 0">
        <li v-for="u in names" :key="u.id">{{ u.displayName }} <span class="muted">({{ u.username }})</span></li>
      </ul>
      <p v-if="names.length > 0 && profile.userCount > names.length" class="muted">…and {{ profile.userCount - names.length }} more.</p>
    </template>
  </ConfirmDialog>
</template>
