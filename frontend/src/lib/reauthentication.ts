import { reactive } from "vue";

/**
 * The "confirm your password" dialog (GH#498): opened when a request answers 403
 * REAUTHENTICATION_REQUIRED, rendered once in the app shell.
 */
export const reauthentication = reactive({ open: false });
