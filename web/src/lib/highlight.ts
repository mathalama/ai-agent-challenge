import type { Block, Finding, Report, Severity, SourceSpan } from './types';
import { SEVERITY } from './labels';

/** Отрезок текста блока [start, end), к которому привязаны находки. */
export interface Mark {
  start: number;
  end: number;
  findingIds: string[];
  /** Точный фрагмент не найден, подсвечен весь блок (или строки таблицы). */
  fallback: boolean;
}

/** Разметка одного блока: для текстовых блоков по тексту, для таблицы по «виртуальному тексту». */
export interface BlockMarks {
  marks: Mark[];
}

const QUOTES: Record<string, string> = {
  '«': '"', '»': '"', '“': '"', '”': '"', '„': '"', '‟': '"', '″': '"',
  '‘': "'", '’': "'", '‚': "'", '′': "'", '`': "'",
  '—': '-', '–': '-', '−': '-', '‑': '-',
  ' ': ' ',
};

/** Нормализация пробелов и кавычек; для PDF ещё склейка слов, перенесённых через дефис. Возвращает карту в исходные индексы. */
export function normalize(text: string, pdf: boolean): { text: string; map: number[] } {
  const out: string[] = [];
  const map: number[] = [];
  let i = 0;
  while (i < text.length) {
    const ch = text[i];
    if (pdf && ch === '-') {
      const m = /^-[ \t]*\r?\n\s*(?=\p{L})/u.exec(text.slice(i, i + 64));
      if (m) {
        i += m[0].length;
        continue;
      }
    }
    if (/\s/.test(ch) || ch === ' ') {
      if (out.length > 0 && out[out.length - 1] !== ' ') {
        out.push(' ');
        map.push(i);
      }
      i += 1;
      continue;
    }
    out.push(QUOTES[ch] ?? ch);
    map.push(i);
    i += 1;
  }
  if (out[out.length - 1] === ' ') {
    out.pop();
    map.pop();
  }
  return { text: out.join(''), map };
}

/** Находит цитату в тексте; возвращает диапазон в исходных индексах текста. */
export function findQuote(text: string, quote: string, pdf: boolean): [number, number] | null {
  const q = normalize(quote, pdf).text;
  if (!q) return null;
  const n = normalize(text, pdf);
  const at = n.text.indexOf(q);
  if (at < 0) return null;
  return [n.map[at], n.map[at + q.length - 1] + 1];
}

const overlaps = (a: SourceSpan, b: SourceSpan) =>
  a.page === b.page && a.start_line <= b.end_line && b.start_line <= a.end_line;

export interface TableLayout {
  /** Виртуальный текст: ячейки строки через пробел, строки через перевод строки. */
  text: string;
  /** cellStarts[row][cell] — смещение ячейки в виртуальном тексте. */
  cellStarts: number[][];
  rowStarts: number[];
  rowEnds: number[];
  rowLines: number[];
}

export function tableLayout(block: Extract<Block, { kind: 'table' }>): TableLayout {
  let text = '';
  const cellStarts: number[][] = [];
  const rowStarts: number[] = [];
  const rowEnds: number[] = [];
  block.rows.forEach((row, r) => {
    if (r > 0) text += '\n';
    rowStarts.push(text.length);
    const starts: number[] = [];
    row.forEach((cell, c) => {
      if (c > 0) text += ' ';
      starts.push(text.length);
      text += cell;
    });
    cellStarts.push(starts);
    rowEnds.push(text.length);
  });
  const rowLines = block.rows.map((_, i) => block.row_lines?.[i] ?? block.location.start_line + i);
  return { text, cellStarts, rowStarts, rowEnds, rowLines };
}

function markBlock(block: Block, finding: Finding): Mark[] {
  const found: Mark[] = [];
  const fallback: Mark[] = [];
  for (const ev of finding.evidence) {
    if (!overlaps(block.location, ev.location)) continue;
    const pdf = ev.location.page !== null;
    if (block.kind === 'table') {
      const lay = tableLayout(block);
      const rows = lay.rowLines.flatMap((line, i) =>
        line >= ev.location.start_line && line <= ev.location.end_line ? [i] : [],
      );
      if (rows.length === 0) continue;
      const from = lay.rowStarts[rows[0]];
      const to = lay.rowEnds[rows[rows.length - 1]];
      const hit = findQuote(lay.text.slice(from, to), ev.quote, pdf);
      if (hit) found.push({ start: from + hit[0], end: from + hit[1], findingIds: [finding.id], fallback: false });
      else fallback.push({ start: from, end: to, findingIds: [finding.id], fallback: true });
    } else {
      const hit = findQuote(block.text, ev.quote, pdf);
      if (hit) found.push({ start: hit[0], end: hit[1], findingIds: [finding.id], fallback: false });
      else fallback.push({ start: 0, end: block.text.length, findingIds: [finding.id], fallback: true });
    }
  }
  return found.length > 0 ? found : fallback;
}

/**
 * Разделяет пересекающиеся метки на элементарные отрезки: в каждом свой набор находок.
 * Так несколько находок на одном месте не ломают разметку.
 */
function split(marks: Mark[]): Mark[] {
  const cuts = [...new Set(marks.flatMap((m) => [m.start, m.end]))].sort((a, b) => a - b);
  const out: Mark[] = [];
  for (let i = 0; i < cuts.length - 1; i++) {
    const [start, end] = [cuts[i], cuts[i + 1]];
    const covering = marks.filter((m) => m.start <= start && m.end >= end);
    if (covering.length === 0) continue;
    out.push({
      start,
      end,
      findingIds: [...new Set(covering.flatMap((m) => m.findingIds))],
      fallback: covering.every((m) => m.fallback),
    });
  }
  return out;
}

/** Метки по блокам: block.id → отрезки. Порядок находок внутри отрезка: от важной к менее важной. */
export function buildMarks(report: Report): Map<number, Mark[]> {
  const bySeverity = [...report.findings].sort(
    (a, b) => SEVERITY[b.severity].rank - SEVERITY[a.severity].rank,
  );
  const result = new Map<number, Mark[]>();
  for (const block of report.blocks) {
    const marks = bySeverity.flatMap((f) => markBlock(block, f));
    if (marks.length > 0) result.set(block.id, split(marks));
  }
  return result;
}

export function worstOf(ids: string[], byId: Map<string, Finding>): Severity {
  return ids.reduce<Severity>((worst, id) => {
    const s = byId.get(id)?.severity ?? 'low';
    return SEVERITY[s].rank > SEVERITY[worst].rank ? s : worst;
  }, 'low');
}

/** Находки, которые делят с выбранной хотя бы один отрезок текста. */
export function siblingsOf(findingId: string, marks: Map<number, Mark[]>): string[] {
  const ids = new Set<string>();
  for (const list of marks.values()) {
    for (const m of list) if (m.findingIds.includes(findingId)) m.findingIds.forEach((id) => ids.add(id));
  }
  ids.delete(findingId);
  return [...ids];
}
