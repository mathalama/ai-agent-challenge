import { useEffect, useMemo, useRef } from 'react';
import { useSearchParams } from 'react-router-dom';
import { FindingCard } from '../components/FindingCard';
import { HighlightedBlock } from '../components/HighlightedBlock';
import { ReportGate } from '../components/ReportGate';
import { SeverityBadge } from '../components/SeverityBadge';
import { Button } from '../components/Button';
import { CRITERIA, SEVERITY, SEVERITY_ORDER } from '../lib/labels';
import { buildMarks, siblingsOf } from '../lib/highlight';
import type { Finding, Report } from '../lib/types';

/** Мигает местом находки в тексте. */
function flash(root: HTMLElement | null, findingId: string, scroll: boolean) {
  if (!root) return;
  const targets = [...root.querySelectorAll<HTMLElement>('[data-finding]')].filter((el) =>
    el.dataset.finding!.split(' ').includes(findingId),
  );
  if (targets.length === 0) return;
  if (scroll) {
    const r = targets[0].getBoundingClientRect();
    const comfortable = r.top > 120 && r.bottom < window.innerHeight - 120;
    if (!comfortable) targets[0].scrollIntoView({ block: 'center', behavior: 'smooth' });
  }
  for (const el of targets) {
    el.animate(
      [{ backgroundColor: 'rgb(36 70 201 / 0.55)' }, { backgroundColor: 'rgb(36 70 201 / 0)' }],
      { duration: 700, iterations: 2, easing: 'ease-out' },
    );
  }
}

function FindingRow({ finding, onClick }: { finding: Finding; onClick: () => void }) {
  return (
    <li>
      <button
        type="button"
        onClick={onClick}
        className="flex w-full cursor-pointer items-center gap-2 rounded-lg px-2 py-1.5 text-left text-sm transition-colors duration-100 hover:bg-sunken"
      >
        <span aria-hidden className={`size-2 shrink-0 rounded-full ${SEVERITY[finding.severity].dot}`} />
        <span className="min-w-0 flex-1 truncate">{CRITERIA[finding.criterion].label}</span>
        <span className="tabular shrink-0 text-xs text-muted">{finding.evidence[0]?.location.start_line}</span>
      </button>
    </li>
  );
}

function DocumentView({ report }: { report: Report }) {
  const [params, setParams] = useSearchParams();
  const selectedId = params.get('finding');
  const docRef = useRef<HTMLDivElement>(null);

  function selectFinding(id: string | null) {
    setParams(id ? { tab: 'document', finding: id } : { tab: 'document' }, { replace: true });
  }

  const byId = useMemo(() => new Map(report.findings.map((f) => [f.id, f])), [report]);
  const marks = useMemo(() => buildMarks(report), [report]);
  const sorted = useMemo(
    () => [...report.findings].sort((a, b) => SEVERITY[b.severity].rank - SEVERITY[a.severity].rank),
    [report],
  );

  useEffect(() => {
    if (!selectedId) return;
    const t = window.setTimeout(() => flash(docRef.current, selectedId, true), 60);
    return () => window.clearTimeout(t);
  }, [selectedId]);

  const selected = selectedId ? (byId.get(selectedId) ?? null) : null;
  const siblings = selected ? siblingsOf(selected.id, marks).flatMap((id) => byId.get(id) ?? []) : [];
  const unplaced = report.findings.filter((f) => ![...marks.values()].some((list) => list.some((m) => m.findingIds.includes(f.id))));

  const ctx = {
    byId,
    selectedId,
    onPick: (ids: string[]) => selectFinding(ids.includes(selectedId ?? '') && ids.length > 1 ? selectedId : ids[0]),
  };

  return (
    <div className="space-y-5">
      <header className="space-y-1">
        <h1 className="text-3xl font-semibold tracking-[-0.02em]">Документ с подсветкой</h1>
        <p className="text-ink-2">
          {report.passport.document_title}. Нажмите на подсвеченное место, чтобы увидеть находку.
        </p>
        <ul className="flex flex-wrap gap-x-4 gap-y-1 pt-1 text-xs text-muted" aria-label="Цвета важности">
          {[...SEVERITY_ORDER].reverse().map((s) => (
            <li key={s} className="flex items-center gap-1.5">
              <span aria-hidden className={`size-2 rounded-full ${SEVERITY[s].dot}`} />
              {SEVERITY[s].label}
            </li>
          ))}
        </ul>
      </header>

      <div className="flex flex-col-reverse gap-5">
        <div ref={docRef} className="min-w-0 rounded-2xl bg-surface px-4 py-4 text-[15px] leading-relaxed ring-1 ring-line">
          {report.blocks.map((block) => (
            <HighlightedBlock key={block.id} block={block} marks={marks.get(block.id) ?? []} ctx={ctx} />
          ))}
        </div>

        <aside
          aria-label="Находка"
          className="space-y-3"
        >
          {selected ? (
            <>
              <div className="flex items-center justify-between gap-2">
                <Button variant="secondary" className="!px-3 !py-1.5" onClick={() => selectFinding(null)}>
                  ← Все находки
                </Button>
                <Button
                  variant="secondary"
                  className="!px-3 !py-1.5"
                  onClick={() => flash(docRef.current, selected.id, true)}
                >
                  Найти в тексте
                </Button>
              </div>
              {siblings.length > 0 && (
                <div className="rounded-xl bg-surface p-3 ring-1 ring-line">
                  <p className="mb-1 px-2 text-xs font-semibold text-muted">
                    На этом месте ещё находок: {siblings.length}
                  </p>
                  <ul>
                    {siblings.map((f) => (
                      <FindingRow key={f.id} finding={f} onClick={() => selectFinding(f.id)} />
                    ))}
                  </ul>
                </div>
              )}
              <FindingCard finding={selected} docLink={false} />
            </>
          ) : (
            <div className="rounded-2xl bg-surface p-4 ring-1 ring-line">
              <h2 className="px-2 pb-1 text-sm font-semibold">Находки ({sorted.length})</h2>
              <ul>
                {sorted.map((f) => (
                  <FindingRow key={f.id} finding={f} onClick={() => selectFinding(f.id)} />
                ))}
              </ul>
              {unplaced.length > 0 && (
                <p className="px-2 pt-2 text-xs text-muted">
                  Без места в тексте: {unplaced.map((f) => f.id).join(', ')}.
                </p>
              )}
            </div>
          )}
          {selected && <SeverityLegendNote finding={selected} />}
        </aside>
      </div>
    </div>
  );
}

function SeverityLegendNote({ finding }: { finding: Finding }) {
  return (
    <p className="flex items-center gap-2 px-1 text-xs text-muted">
      <SeverityBadge severity={finding.severity} />
      Цвет подсветки в тексте соответствует важности находки.
    </p>
  );
}

export function DocumentScreen() {
  return <ReportGate>{(report) => <DocumentView report={report} />}</ReportGate>;
}
