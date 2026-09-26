/**
 * The icons an administrator can give a CI class (ci_classes.icon stores the key).
 * Each is a set of SVG path strings on a 16×16 grid, stroked in currentColor.
 * An icon key the UI does not know (set through the API) is kept and shown as text in
 * the editor, and renders no glyph.
 */
export interface ClassIcon {
  key: string;
  label: string;
  paths: string[];
}

export const CLASS_ICONS: ClassIcon[] = [
  { key: "server", label: "Server", paths: ["M2.5 2.5h11v4h-11z", "M2.5 9.5h11v4h-11z", "M5 4.5h.01", "M5 11.5h.01"] },
  { key: "vm", label: "Virtual machine", paths: ["M2.5 3.5h11v8h-11z", "M5.5 14h5", "M8 11.5V14", "M5 6l3 2.5L11 6"] },
  { key: "network", label: "Network device", paths: ["M1.5 8.5h13v4h-13z", "M4 10.5h.01", "M7 10.5h.01", "M8 8.5v-3", "M5 3.5l3 2 3-2"] },
  { key: "storage", label: "Storage", paths: ["M3 3.5c0-1 10-1 10 0v9c0 1-10 1-10 0z", "M3 3.5c0 1 10 1 10 0", "M3 8c0 1 10 1 10 0"] },
  { key: "database", label: "Database", paths: ["M3 3c0-1.3 10-1.3 10 0v10c0 1.3-10 1.3-10 0z", "M3 3c0 1.3 10 1.3 10 0", "M3 6.5c0 1.3 10 1.3 10 0", "M3 10c0 1.3 10 1.3 10 0"] },
  { key: "application", label: "Application", paths: ["M2.5 2.5h11v11h-11z", "M2.5 5.5h11", "M4.5 4h.01", "M6.5 4h.01"] },
  { key: "service", label: "Service", paths: ["M8 2.5l5.5 3v5L8 13.5l-5.5-3v-5z", "M8 8l5.5-2.5", "M8 8v5.5", "M8 8L2.5 5.5"] },
  { key: "container", label: "Container", paths: ["M1.5 5.5h13v7h-13z", "M4.5 5.5v7", "M8 5.5v7", "M11.5 5.5v7", "M3 5.5l1-2h8l1 2"] },
  { key: "cloud", label: "Cloud", paths: ["M4.5 12.5a3 3 0 010-6 4 4 0 017.6-1A3 3 0 0112 12.5z"] },
  { key: "location", label: "Location", paths: ["M8 14.5s-4.5-4.2-4.5-7.5a4.5 4.5 0 019 0c0 3.3-4.5 7.5-4.5 7.5z", "M8 7h.01"] },
  { key: "device", label: "Device", paths: ["M4.5 1.5h7v13h-7z", "M7 12.5h2"] },
  { key: "document", label: "Document", paths: ["M3.5 1.5h6l3 3v10h-9z", "M9.5 1.5v3h3", "M5.5 8h5", "M5.5 10.5h5"] },
  { key: "person", label: "Person", paths: ["M8 7.5a2.5 2.5 0 100-5 2.5 2.5 0 000 5z", "M3 14c0-3 2.2-4.5 5-4.5s5 1.5 5 4.5"] },
  { key: "generic", label: "Generic item", paths: ["M3 3h10v10H3z"] },
];

const BY_KEY = new Map(CLASS_ICONS.map((i) => [i.key, i]));

export function classIcon(key: string | null | undefined): ClassIcon | undefined {
  return key ? BY_KEY.get(key) : undefined;
}
