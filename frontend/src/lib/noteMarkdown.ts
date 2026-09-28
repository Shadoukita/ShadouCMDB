/**
 * The limited Markdown of layout notes: paragraphs, line breaks, bulleted and
 * numbered lists, **bold**, *italic*, `code` and [links](https://…). The result
 * is a tree of plain values that the NoteText component renders with ordinary
 * elements and text nodes, so HTML in a note is shown as text, never rendered.
 * Links only keep http, https and mailto targets; any other scheme (javascript:,
 * data:) is shown as text.
 */

export type Inline =
  | { t: "text" | "strong" | "em" | "code"; text: string }
  | { t: "link"; text: string; href: string };
export type Block = { t: "p"; lines: Inline[][] } | { t: "ul" | "ol"; items: Inline[][] };

const BULLET = /^\s*[-*+]\s+(.*)$/;
const NUMBERED = /^\s*\d{1,9}[.)]\s+(.*)$/;
const INLINE = /\*\*([^*\n]+)\*\*|__([^_\n]+)__|\*([^*\s][^*\n]*)\*|_([^_\s][^_\n]*)_|`([^`\n]+)`|\[([^\]\n]+)\]\(([^)\s]+)\)/g;
const SAFE_LINK = /^(https?:\/\/|mailto:)/i;

export function parseInline(line: string): Inline[] {
  const out: Inline[] = [];
  let at = 0;
  for (const m of line.matchAll(INLINE)) {
    if (m.index > at) out.push({ t: "text", text: line.slice(at, m.index) });
    if (m[1] ?? m[2]) out.push({ t: "strong", text: (m[1] ?? m[2])! });
    else if (m[3] ?? m[4]) out.push({ t: "em", text: (m[3] ?? m[4])! });
    else if (m[5]) out.push({ t: "code", text: m[5] });
    else if (SAFE_LINK.test(m[7])) out.push({ t: "link", text: m[6], href: m[7] });
    else out.push({ t: "text", text: m[0] });
    at = m.index + m[0].length;
  }
  if (at < line.length) out.push({ t: "text", text: line.slice(at) });
  return out;
}

export function parseNote(text: string): Block[] {
  const blocks: Block[] = [];
  /** The block the next line continues, if any (a blank line ends it). */
  let open = false;
  const last = () => blocks[blocks.length - 1];
  for (const raw of text.replace(/\r\n?/g, "\n").split("\n")) {
    if (raw.trim() === "") {
      open = false;
      continue;
    }
    const bullet = BULLET.exec(raw);
    const numbered = bullet ? null : NUMBERED.exec(raw);
    const item = (bullet ?? numbered)?.[1];
    const cur = open ? last() : undefined;
    if (item !== undefined) {
      const list = bullet ? "ul" : "ol";
      if (cur?.t === list) cur.items.push(parseInline(item));
      else blocks.push({ t: list, items: [parseInline(item)] });
    } else if (cur && cur.t !== "p") {
      // A line under a list item continues it.
      cur.items[cur.items.length - 1].push({ t: "text", text: " " }, ...parseInline(raw.trim()));
    } else if (cur?.t === "p") cur.lines.push(parseInline(raw));
    else blocks.push({ t: "p", lines: [parseInline(raw)] });
    open = true;
  }
  return blocks;
}
