import { Fragment, type ReactNode } from 'react';
import type { Block, Finding } from '../lib/types';
import { tableLayout, worstOf, type Mark } from '../lib/highlight';

const MARK_CLASS = {
  low: 'bg-sev-low/20 shadow-[inset_0_-2px_0_var(--color-sev-low)]',
  medium: 'bg-sev-medium/25 shadow-[inset_0_-2px_0_var(--color-sev-medium)]',
  high: 'bg-sev-high/25 shadow-[inset_0_-2px_0_var(--color-sev-high)]',
  critical: 'bg-sev-critical/25 shadow-[inset_0_-2px_0_var(--color-sev-critical)]',
} as const;

interface Ctx {
  byId: Map<string, Finding>;
  selectedId: string | null;
  onPick: (ids: string[]) => void;
}

/** Текст [base, base + text.length) блока с метками. Метки вне диапазона отбрасываются, граничные обрезаются. */
function renderRange(text: string, base: number, marks: Mark[], ctx: Ctx): ReactNode {
  const nodes: ReactNode[] = [];
  let pos = 0;
  for (const m of marks) {
    const start = Math.max(m.start - base, 0);
    const end = Math.min(m.end - base, text.length);
    if (end <= start || start < pos) continue;
    if (start > pos) nodes.push(<Fragment key={`t${pos}`}>{text.slice(pos, start)}</Fragment>);
    const selected = ctx.selectedId !== null && m.findingIds.includes(ctx.selectedId);
    nodes.push(
      <mark
        key={`m${start}`}
        data-finding={m.findingIds.join(' ')}
        role="button"
        tabIndex={0}
        aria-label={`Находок на этом месте: ${m.findingIds.length}`}
        onClick={() => ctx.onPick(m.findingIds)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            ctx.onPick(m.findingIds);
          }
        }}
        className={`cursor-pointer rounded-[3px] px-0.5 text-inherit transition-[background-color,box-shadow] duration-150 hover:brightness-95 ${
          MARK_CLASS[worstOf(m.findingIds, ctx.byId)]
        } ${m.fallback ? 'opacity-80' : ''} ${selected ? 'outline-2 outline-offset-1 outline-ink' : ''}`}
      >
        {text.slice(start, end)}
        {m.findingIds.length > 1 && (
          <sup className="ml-0.5 text-[10px] font-semibold text-ink-2">{m.findingIds.length}</sup>
        )}
      </mark>,
    );
    pos = end;
  }
  if (pos < text.length) nodes.push(<Fragment key={`t${pos}`}>{text.slice(pos)}</Fragment>);
  return nodes;
}

const HEADING_CLASS = [
  '',
  'text-2xl font-semibold tracking-[-0.02em] mt-6',
  'text-xl font-semibold tracking-[-0.01em] mt-5',
  'text-lg font-semibold mt-4',
  'text-base font-semibold mt-3',
  'text-base font-semibold mt-3',
  'text-base font-semibold mt-3',
];

function Table({ block, marks, ctx }: { block: Extract<Block, { kind: 'table' }>; marks: Mark[]; ctx: Ctx }) {
  const lay = tableLayout(block);
  return (
    <div className="overflow-x-auto rounded-lg ring-1 ring-line">
      <table className="w-full border-collapse text-sm">
        <tbody>
          {block.rows.map((row, r) => (
            <tr key={r} className={r === 0 ? 'bg-sunken font-semibold' : 'border-t border-line'}>
              {row.map((cell, c) => (
                <td key={c} className="px-3 py-1.5 align-top">
                  {renderRange(cell, lay.cellStarts[r][c], marks, ctx)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export function HighlightedBlock({ block, marks, ctx }: { block: Block; marks: Mark[]; ctx: Ctx }) {
  const content = (() => {
    switch (block.kind) {
      case 'heading':
        return (
          <p role="heading" aria-level={Math.min(block.level + 1, 6)} className={HEADING_CLASS[Math.min(block.level, 6)]}>
            {renderRange(block.text, 0, marks, ctx)}
          </p>
        );
      case 'paragraph':
        return <p className="text-ink-2">{renderRange(block.text, 0, marks, ctx)}</p>;
      case 'list_item':
        return (
          <p className="flex gap-2 pl-2 text-ink-2">
            <span aria-hidden className="text-muted">•</span>
            <span>{renderRange(block.text, 0, marks, ctx)}</span>
          </p>
        );
      case 'code':
        return (
          <pre className="overflow-x-auto rounded-lg bg-sunken px-3 py-2 font-mono text-[13px] leading-relaxed whitespace-pre-wrap">
            {renderRange(block.text, 0, marks, ctx)}
          </pre>
        );
      case 'table':
        return <Table block={block} marks={marks} ctx={ctx} />;
    }
  })();

  return (
    <div data-block={block.id} className="grid grid-cols-[2.25rem_1fr] gap-2 py-0.5">
      <span aria-hidden className="tabular pt-1 text-right text-[11px] text-muted/70 select-none">
        {block.location.start_line}
      </span>
      <div className="min-w-0">{content}</div>
    </div>
  );
}
