import type { IconName } from "../icons/lucide";

/**
 * The icons an administrator can give a CI class (ci_classes.icon stores the key).
 * Each maps to an icon of the vendored Lucide set, drawn in currentColor.
 * An icon key the UI does not know (set through the API) is kept and shown as text in
 * the editor, and renders no glyph.
 */
export interface ClassIcon {
  key: string;
  label: string;
  icon: IconName;
}

export const CLASS_ICONS: ClassIcon[] = [
  { key: "server", label: "Server", icon: "server" },
  { key: "vm", label: "Virtual machine", icon: "monitor" },
  { key: "network", label: "Network device", icon: "network" },
  { key: "storage", label: "Storage", icon: "hard-drive" },
  { key: "database", label: "Database", icon: "database" },
  { key: "application", label: "Application", icon: "app-window" },
  { key: "service", label: "Service", icon: "box" },
  { key: "container", label: "Container", icon: "container" },
  { key: "cloud", label: "Cloud", icon: "cloud" },
  { key: "location", label: "Location", icon: "map-pin" },
  { key: "device", label: "Device", icon: "smartphone" },
  { key: "document", label: "Document", icon: "file-text" },
  { key: "person", label: "Person", icon: "user" },
  { key: "generic", label: "Generic item", icon: "square" },
];

const BY_KEY = new Map(CLASS_ICONS.map((i) => [i.key, i]));

export function classIcon(key: string | null | undefined): ClassIcon | undefined {
  return key ? BY_KEY.get(key) : undefined;
}
