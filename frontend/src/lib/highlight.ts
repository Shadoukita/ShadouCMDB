/** One run of a highlighted string: matched text, or the text between matches. */
export interface HighlightPart {
  text: string;
  match: boolean;
}

/**
 * Splits `text` into runs around every case-insensitive occurrence of the search term's words, so a template
 * can wrap the matches in <mark> (audit Q2). Plain strings only, never HTML: the caller renders each run as text.
 * Words shorter than two characters are ignored; overlapping matches merge.
 */
export function highlight(text: string, term: string): HighlightPart[] {
  const words = term
    .toLocaleLowerCase()
    .split(/\s+/)
    .filter((w) => w.length >= 2);
  if (!text || words.length === 0) return [{ text, match: false }];
  const lower = text.toLocaleLowerCase();
  // Matched character ranges; toLocaleLowerCase can change the length of some characters, so fall back to no highlight then.
  if (lower.length !== text.length) return [{ text, match: false }];
  const hit = new Array<boolean>(text.length).fill(false);
  for (const w of words) {
    for (let at = lower.indexOf(w); at !== -1; at = lower.indexOf(w, at + 1)) hit.fill(true, at, at + w.length);
  }
  const parts: HighlightPart[] = [];
  for (let i = 0; i < text.length; i++) {
    const last = parts[parts.length - 1];
    if (last && last.match === hit[i]) last.text += text[i];
    else parts.push({ text: text[i], match: hit[i] });
  }
  return parts;
}
