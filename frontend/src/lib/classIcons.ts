import { t, type MessageKey } from "../i18n/index";
import type { IconName } from "../icons/lucide";

/**
 * The icons an administrator can give a CI class (ci_classes.icon stores the key).
 * Each maps to an icon of the vendored Lucide set, drawn in currentColor.
 * An icon key the UI does not know (set through the API) is kept and shown as text in
 * the editor, and renders no glyph.
 */
export interface ClassIcon {
  key: string;
  readonly label: string;
  icon: IconName;
}

/** The label is read on access, so it follows the active locale. */
function icon(key: string, labelKey: MessageKey, glyph: IconName): ClassIcon {
  return {
    key,
    get label() {
      return t(labelKey);
    },
    icon: glyph,
  };
}

export const CLASS_ICONS: ClassIcon[] = [
  icon("server", "dm.icon.server", "server"),
  icon("vm", "dm.icon.vm", "monitor"),
  icon("network", "dm.icon.network", "network"),
  icon("storage", "dm.icon.storage", "hard-drive"),
  icon("database", "dm.icon.database", "database"),
  icon("application", "dm.icon.application", "app-window"),
  icon("service", "dm.icon.service", "box"),
  icon("container", "dm.icon.container", "container"),
  icon("cloud", "dm.icon.cloud", "cloud"),
  icon("location", "dm.icon.location", "map-pin"),
  icon("device", "dm.icon.device", "smartphone"),
  icon("document", "dm.icon.document", "file-text"),
  icon("person", "dm.icon.person", "user"),
  icon("generic", "dm.icon.generic", "square"),
];

const BY_KEY = new Map(CLASS_ICONS.map((i) => [i.key, i]));

export function classIcon(key: string | null | undefined): ClassIcon | undefined {
  return key ? BY_KEY.get(key) : undefined;
}
