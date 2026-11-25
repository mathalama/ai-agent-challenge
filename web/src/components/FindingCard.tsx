import { forwardRef, type ReactNode } from 'react';
import { Link } from 'react-router-dom';
import { BorderBeam } from 'border-beam';
import { CRITERIA, DETECTOR_KIND, STAGE_BY_ID, findingStage } from '../lib/labels';
import type { Evidence, Finding } from '../lib/types';
import { SeverityBadge } from './SeverityBadge';

export function formatLocation(loc: Evidence['location']): string {
  const lines =
    loc.start_line === loc.end_line ? `строка ${loc.start_line}` : `строки ${loc.start_line}–${loc.end_line}`;
  return loc.page === null ? lines : `стр. ${loc.page}, ${lines}`;
}

function Quote({ evidence, label }: { evidence: Evidence; label?: string }) {
  return (
    <figure className="min-w-0 rounded-lg bg-sunken px-3 py-2">
      {label && <p className="mb-1 text-[11px] font-semibold tracking-wide text-muted uppercase">{label}</p>}
      <blockquote className="text-sm break-words whitespace-pre-wrap text-ink">«{evidence.quote}»</blockquote>
      <figcaption className="tabular mt-1 text-xs text-muted">{formatLocation(evidence.location)}</figcaption>
    </figure>
  );
}

function detectorText(f: Finding): string {
  const kind = DETECTOR_KIND[f.detector.kind] ?? f.detector.kind;
  if (f.detector.kind === 'rule') return `${kind}: ${f.detector.source}`;
  return `${kind}: ${STAGE_BY_ID[findingStage(f)].label.toLowerCase()}`;
}

interface Props {
  finding: Finding;
  selected?: boolean;
  /** Показать ссылку «Показать в документе». На экране документа она не нужна. */
  docLink?: boolean;
  onSelect?: () => void;
  actions?: ReactNode;
}

export const FindingCard = forwardRef<HTMLElement, Props>(function FindingCard(
  { finding, selected = false, docLink = false, onSelect, actions },
  ref,
) {
  const c = CRITERIA[finding.criterion];
  const contradiction = finding.criterion === 'consistent' && finding.evidence.length === 2;

  const card = (
    <article
      ref={ref}
      id={`finding-${finding.id}`}
      onClick={onSelect}
      className={`space-y-3 rounded-2xl bg-surface p-5 ring-1 transition-shadow duration-150 ${
        selected ? 'ring-2 ring-accent' : 'ring-line'
      } ${onSelect ? 'cursor-pointer' : ''}`}
    >
      <header className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
        <SeverityBadge severity={finding.severity} />
        <h3 className="text-[15px] font-semibold">
          {c.label}
          <span className="font-normal text-muted"> · {c.standard}</span>
        </h3>
      </header>

      <p className="text-ink">{finding.explanation}</p>

      {contradiction ? (
        <div className="grid gap-2 sm:grid-cols-2">
          <Quote evidence={finding.evidence[0]} label="Утверждение 1" />
          <Quote evidence={finding.evidence[1]} label="Утверждение 2" />
        </div>
      ) : (
        <div className="space-y-2">
          {finding.evidence.map((ev, i) => (
            <Quote key={i} evidence={ev} />
          ))}
        </div>
      )}

      {finding.suggestion && (
        <p className="rounded-lg bg-accent-soft px-3 py-2 text-sm text-ink">
          <span className="font-semibold text-accent-ink">Как исправить. </span>
          {finding.suggestion}
        </p>
      )}

      <footer className="flex flex-wrap items-center gap-x-4 gap-y-1.5 text-xs text-muted">
        <span
          className={`inline-flex items-center gap-1 font-medium ${finding.verified ? 'text-ok-ink' : 'text-sev-medium-ink'}`}
        >
          <span aria-hidden>{finding.verified ? '✓' : '!'}</span>
          {finding.verified ? 'цитата подтверждена' : 'цитата не подтверждена'}
        </span>
        <span>{detectorText(finding)}</span>
        <span>
          {finding.scope.kind === 'requirement' ? (
            <Link
              to={`/findings?requirement=${encodeURIComponent(finding.scope.id)}`}
              onClick={(e) => e.stopPropagation()}
              className="text-accent-ink hover:underline"
            >
              Требование {finding.scope.id}
            </Link>
          ) : (
            'Весь набор требований'
          )}
        </span>
        <span className="tabular">{finding.id}</span>
        {docLink && (
          <Link
            to={`/document?finding=${encodeURIComponent(finding.id)}`}
            onClick={(e) => e.stopPropagation()}
            className="ml-auto font-semibold text-accent-ink hover:underline"
          >
            Показать в документе
          </Link>
        )}
        {actions}
      </footer>
    </article>
  );

  return finding.severity === 'critical' ? (
    <BorderBeam size="md" colorVariant="sunset" theme="light" strength={0.85}>
      {card}
    </BorderBeam>
  ) : (
    card
  );
});
