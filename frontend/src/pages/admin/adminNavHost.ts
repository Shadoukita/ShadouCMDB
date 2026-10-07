import { ref } from "vue";

/**
 * Whether the rail shows the Administration sub-navigation right now (MainNav sets it). AdminLayout
 * shows its own copy beside the page only when the rail does not: collapsed to icons, or with
 * Administration hidden from the navigation in Customization.
 */
export const adminNavInRail = ref(false);
