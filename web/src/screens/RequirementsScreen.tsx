import { useMemo, useState } from 'react';
import { Link, useNavigate } from 'react-router-dom';
import { ReportGate } from '../components/ReportGate';
import { SeverityBadge } from '../components/SeverityBadge';
import { SEVERITY } from '../lib/labels';
import { findingsForRequirement, worstSeverity } from '../lib/selectors';
import type { Report } from '../lib/types';

type SortKey = 'id' | 'source' | 'text' | 'count' | 'worst';
interface Row {
  id: string;
  source: string;
  text: string;
  count: number;
  worst: number; // -1 — находок нет
  worstSeverity: ReturnType<typeof worstSeverity>;
}

const COLUMNS: { key: SortKey; label: string; className?: string }[] = [
  { key: 'id', label: 'ID' },
  { key: 'source', label: 'В документе' },
  { key: 'text', label: 'Формулировка' },
  { key: 'count', label: 'Находок', className: 'text-right' },
  { key: 'worst', label: 'Худшая важность' },
];

const collator = new Intl.Collator('ru', { numeric: true });

function Content({ report }: { report: Report }) {
  const navigate = useNavigate();
  const [sort, setSort] = useState<{ key: SortKey; dir: 'asc' | 'desc' }>({ key: 'id', dir: 'asc' });

  const rows = useMemo<Row[]>(
    () =>
      report.requirements.map((r) => {
        const found = findingsForRequirement(report, r);
        const worst = worstSeverity(found);
        return {
          id: r.id,
          source: r.source_id ?? '',
          text: r.text,
          count: found.length,
          worst: worst ? SEVERITY[worst].rank : -1,
          worstSeverity: worst,
        };
      }),
    [report],
  );

  const sorted = useMemo(() => {
    const sign = sort.dir === 'asc' ? 1 : -1;
    return [...rows].sort((a, b) => {
      const av = a[sort.key];
      const bv = b[sort.key];
      // Пустой ID в документе всегда внизу, чтобы не засорять сортировку.
      if (sort.key === 'source' && (av === '') !== (bv === '')) return av === '' ? 1 : -1;
      const cmp = typeof av === 'number' && typeof bv === 'number' ? av - bv : collator.compare(String(av), String(bv));
      return cmp * sign || collator.compare(a.id, b.id);
    });
  }, [rows, sort]);

  const toggle = (key: SortKey) =>
    setSort((s) => (s.key === key ? { key, dir: s.dir === 'asc' ? 'desc' : 'asc' } : { key, dir: key === 'count' || key === 'worst' ? 'desc' : 'asc' }));

  return (
    <div className="space-y-6">
      <div className="space-y-1">
        <h1 className="text-3xl font-semibold tracking-[-0.025em]">Требования</h1>
        <p className="text-ink-2">Выделено {rows.length}. Выберите требование, чтобы увидеть его находки.</p>
      </div>
      <div className="overflow-x-auto rounded-2xl border border-line bg-surface">
        <table className="w-full min-w-[46rem] border-collapse text-left text-sm">
          <thead>
            <tr className="border-b border-line bg-ground/60">
              {COLUMNS.map((c) => {
                const active = sort.key === c.key;
                return (
                  <th key={c.key} scope="col" aria-sort={active ? (sort.dir === 'asc' ? 'ascending' : 'descending') : 'none'} className={`px-4 py-2.5 font-medium text-muted ${c.className ?? ''}`}>
                    <button type="button" onClick={() => toggle(c.key)} className={`inline-flex cursor-pointer items-center gap-1 rounded hover:text-ink ${active ? 'text-ink' : ''}`}>
                      {c.label}
                      <span aria-hidden className={`text-xs ${active ? 'opacity-100' : 'opacity-0'}`}>{sort.dir === 'asc' ? '↑' : '↓'}</span>
                    </button>
                  </th>
                );
              })}
            </tr>
          </thead>
          <tbody className="divide-y divide-line">
            {sorted.map((r) => (
              <tr key={r.id} onClick={() => navigate(`/?tab=findings&requirement=${encodeURIComponent(r.id)}`)} className="cursor-pointer transition-colors duration-100 hover:bg-accent-soft/60">
                <td className="px-4 py-3 font-mono text-[13px] whitespace-nowrap">
                  <Link to={`/?tab=findings&requirement=${encodeURIComponent(r.id)}`} onClick={(e) => e.stopPropagation()} className="font-medium text-accent-ink hover:underline">{r.id}</Link>
                </td>
                <td className="px-4 py-3 whitespace-nowrap text-ink-2">{r.source || '—'}</td>
                <td className="max-w-[30rem] px-4 py-3"><p className="line-clamp-2">{r.text}</p></td>
                <td className="tabular px-4 py-3 text-right font-semibold">{r.count}</td>
                <td className="px-4 py-3">{r.worstSeverity ? <SeverityBadge severity={r.worstSeverity} /> : <span className="text-muted">—</span>}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

export function RequirementsScreen() {
  return <ReportGate>{(report) => <Content report={report} />}</ReportGate>;
}
