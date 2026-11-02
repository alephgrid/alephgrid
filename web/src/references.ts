/** A1 reference editor, not an expression evaluator. Never rewrites quoted text or names. */
interface ReferenceToken { start: number; end: number; row: number; column: number; absoluteRow: boolean; absoluteColumn: boolean; qualified: boolean }
const identifier = (c: string | undefined) => !!c && /[\p{L}\p{N}_.]/u.test(c);

export function referenceTokens(text: string): ReferenceToken[] {
  if (!text.startsWith("=")) return [];
  const out: ReferenceToken[] = [];
  for (let i = 1; i < text.length;) {
    const ch = text[i];
    if (ch === '"' || ch === "'") {
      const quote = ch; i++;
      while (i < text.length) {
        if (text[i++] === quote) { if (text[i] === quote) { i++; continue; } break; }
      }
      continue;
    }
    if (ch === '[') { const end = text.indexOf(']', i + 1); i = end < 0 ? text.length : end + 1; continue; }
    if (identifier(text[i - 1]) || text[i - 1] === '$') { i++; continue; }
    const m = /^(\$?)([A-Za-z]{1,3})(\$?)([1-9][0-9]*)/.exec(text.slice(i));
    if (!m || identifier(text[i + m[0].length]) || /[(!]/.test(text.slice(i + m[0].length).trimStart()[0] ?? "")) { i++; continue; }
    let column = 0;
    for (const c of m[2].toUpperCase()) column = column * 26 + c.charCodeAt(0) - 64;
    const row = Number(m[4]);
    if (column <= 16384 && row <= 1048576) out.push({ start: i, end: i + m[0].length, row: row - 1, column: column - 1, absoluteRow: !!m[3], absoluteColumn: !!m[1], qualified: text[i - 1] === '!' });
    i += m[0].length;
  }
  return out;
}

export function lockReferences(text: string, start: number, end = start): { text: string; start: number; end: number } | null {
  const tokens = referenceTokens(text).filter(t => start === end ? t.start <= start && t.end >= start : t.start < end && t.end > start);
  if (!tokens.length) return null;
  let result = text, delta = 0;
  for (const t of tokens) {
    const letters = text.slice(t.start, t.end).replace(/[$0-9]/g, "");
    // Relative → both absolute → row absolute → column absolute → relative.
    const [col, row] = !t.absoluteColumn && !t.absoluteRow ? [true, true]
      : t.absoluteColumn && t.absoluteRow ? [false, true]
      : t.absoluteRow ? [true, false] : [false, false];
    const replacement = `${col ? '$' : ''}${letters}${row ? '$' : ''}${t.row + 1}`;
    result = result.slice(0, t.start + delta) + replacement + result.slice(t.end + delta);
    delta += replacement.length - (t.end - t.start);
  }
  return { text: result, start: tokens[0].start, end: tokens[tokens.length - 1].end + delta };
}

export function applyReferenceLock(input: HTMLInputElement): boolean {
  const changed = lockReferences(input.value, input.selectionStart ?? 0, input.selectionEnd ?? 0);
  if (!changed) return false;
  input.value = changed.text;
  input.setSelectionRange(changed.start, changed.end);
  input.dispatchEvent(new Event("input", { bubbles: true }));
  return true;
}

/** Replace a selected reference or insert an operand; never splice into literals/names. */
export function referenceInsertion(text: string, start: number, end: number): { start: number; end: number } | null {
  if (!text.startsWith("=")) return null;
  let quote = "";
  for (let i = 1; i < start; i++) {
    if (quote && text[i] === quote && text[i + 1] === quote) { i++; continue; }
    if (quote && text[i] === quote) quote = "";
    else if (!quote && (text[i] === '"' || text[i] === "'")) quote = text[i];
  }
  if (quote) return null;
  const refs = referenceTokens(text);
  const selected = refs.filter(t => start === end ? t.start <= start && t.end >= start : t.start < end && t.end > start);
  if (selected.length) {
    const first = selected[0], last = selected[selected.length - 1];
    if (start >= first.start && end <= last.end && /^\$?[A-Za-z]+\$?\d+(?:\s*:\s*\$?[A-Za-z]+\$?\d+)?$/.test(text.slice(first.start, last.end)) && !first.qualified) return { start: first.start, end: last.end };
  }
  if (start === end && /[=(,+\-*/^&<>:]\s*$/.test(text.slice(0, start))) return { start, end };
  return null;
}

export function referenceRanges(text: string): { r0: number; c0: number; r1: number; c1: number }[] {
  const tokens = referenceTokens(text), ranges = [];
  for (let i = 0; i < tokens.length && ranges.length < 128; i++) {
    const a = tokens[i]; let b = a;
    if (tokens[i + 1] && /^\s*:\s*$/.test(text.slice(a.end, tokens[i + 1].start))) b = tokens[++i];
    if (a.qualified || b.qualified) continue;
    ranges.push({ r0: Math.min(a.row, b.row), c0: Math.min(a.column, b.column), r1: Math.max(a.row, b.row), c1: Math.max(a.column, b.column) });
  }
  return ranges;
}
