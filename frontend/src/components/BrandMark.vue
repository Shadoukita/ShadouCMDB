<script setup lang="ts">
import { computed } from "vue";
import { useBrandingStore } from "../stores/branding";

/**
 * Logo and app name from Customization › Branding (header, sign-in and setup pages). Without a logo, a
 * tile with the name's first letter on the accent colour stands in, so the collapsed rail still shows a mark.
 * The logo and tile are decorative (empty alt, aria-hidden): the name next to them is the text.
 */
const branding = useBrandingStore();
const initial = computed(() => Array.from(branding.effective.appName.trim())[0]?.toLocaleUpperCase() ?? "");
</script>

<template>
  <span class="brand-mark">
    <img v-if="branding.effective.logoUrl" :src="branding.effective.logoUrl" alt="" class="brand-logo" />
    <span v-else class="brand-tile" aria-hidden="true">{{ initial }}</span>
    <span class="brand-name">{{ branding.effective.appName }}</span>
  </span>
</template>
