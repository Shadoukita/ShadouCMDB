import { defineStore } from "pinia";
import { ref } from "vue";

export type Density = "standard" | "comfortable";
export const DENSITIES: readonly Density[] = ["standard", "comfortable"];
const DENSITY_KEY = "shadoucmdb.density";

/**
 * Row and control heights: "standard" (the default, dense tables) or "comfortable"
 * (taller rows, more padding). A per-browser choice like the theme; text sizes do not change.
 * The tokens are in styles/tokens.css under :root[data-density="comfortable"].
 */
export const useDensityStore = defineStore("density", () => {
  const density = ref<Density>(readDensity());

  function setDensity(d: Density) {
    density.value = d;
    try {
      if (d === "standard") localStorage.removeItem(DENSITY_KEY);
      else localStorage.setItem(DENSITY_KEY, d);
    } catch {
      // Storage disabled: the choice lasts for this page load.
    }
  }

  return { density, setDensity };
});

function readDensity(): Density {
  try {
    return localStorage.getItem(DENSITY_KEY) === "comfortable" ? "comfortable" : "standard";
  } catch {
    return "standard";
  }
}

/** Sets <html data-density>; the standard density is the stylesheet's default, so it removes the attribute. */
export function applyDensity(d: Density) {
  const root = document.documentElement;
  if (d === "standard") delete root.dataset.density;
  else root.dataset.density = d;
}
