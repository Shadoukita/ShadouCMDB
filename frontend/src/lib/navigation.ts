import type { Router } from "vue-router";

let navigations = 0;

export function trackNavigations(router: Router) {
  router.afterEach(() => {
    navigations++;
  });
}

/**
 * True while showing a page the operator navigated to inside the app (a link,
 * the menu), false on the first page load (reload, bookmark) and on Back/Forward,
 * where the URL should show exactly what they left.
 */
export function isInAppNavigation(): boolean {
  return navigations > 1 && !(window.history.state as { forward?: unknown } | null)?.forward;
}

/** True on Back/Forward: the page should show the URL exactly as it was left (no default view written over it). */
export function isHistoryNavigation(): boolean {
  return navigations > 1 && !!(window.history.state as { forward?: unknown } | null)?.forward;
}
