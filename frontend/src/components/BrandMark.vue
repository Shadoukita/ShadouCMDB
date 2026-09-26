<script setup lang="ts">
import { useBrandingStore } from "../stores/branding";

/** Logo and app name from Customization › Branding (header, sign-in and setup pages). */
const branding = useBrandingStore();
</script>

<template>
  <span class="brand-mark">
    <img v-if="branding.effective.logoUrl" :src="branding.effective.logoUrl" alt="" class="brand-logo" />
    <span class="brand-name">{{ branding.effective.appName }}</span>
  </span>
</template>
