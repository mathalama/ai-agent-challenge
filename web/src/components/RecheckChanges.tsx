import { Link } from 'react-router-dom';
import { CRITERIA, READINESS, SEVERITY, SEVERITY_ORDER } from '../lib/labels';
import { countBySeverity } from '../lib/selectors';
import type { Finding, Report } from '../lib/types';
import { FindingCard } from './FindingCard';
import { SeverityBadge } from './SeverityBadge';

export interface Diff {
  closed: Finding[];
  remained: Finding[];
  added: Finding[];
}

/** Сравнение двух отчётов по id находок. */
export function diffReports(before: Report, after: Report): Diff {
  const beforeIds = new Set(before.findings.map((f) => f.id));
  const afterIds = new Set(after.findings.map((f) => f.id));
  return {
    closed: before.findings.filter((f) => !afterIds.has(f.id)),
    remained: after.findings.filter((f) => beforeIds.has(f.id)),
    added: after.findings.filter((f) => !beforeIds.has(f.id)),
  };
}

function Verdict({ report }: { report: Report }) {
  const r = READINESS[report.readiness];
  return (
    <span className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-semibold ${r.bg} ${r.text}`}>
      <span aria-hidden className={`size-1.5 rounded-full ${r.dot}`} />
      {r.label}
    </span>
  );
}

function Section({ title, count, tone, children }: { title: string; count: number; tone: string; children?: React.ReactNode }) {
  return (
    <section className="space-y-2">
      <h3 className="flex items-center gap-2 text-lg font-semibold">
        {title}
        <span className={`tabular rounded-full px-2 text-sm ${tone}`}>{count}</span>
      </h3>
      {children}
    </section>
  );
}

function Row({ finding, struck = false }: { finding: Finding; struck?: boolean }) {
  return (
    <li className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-xl bg-surface px-4 py-3 ring-1 ring-line">
      <SeverityBadge severity={finding.severity} />
      <span className={`font-medium ${struck ? 'text-muted line-through' : ''}`}>{CRITERIA[finding.criterion].label}</span>
      <span className="tabular text-xs text-muted">{finding.id}</span>
      <p className="basis-full text-sm text-ink-2">{finding.explanation}</p>
      {!struck && (
        <Link className="text-sm text-accent-ink hover:underline" to={`/document?finding=${encodeURIComponent(finding.id)}`}>
          Показать в документе
        </Link>
      )}
    </li>
  );
}

export function RecheckChanges({ before, after }: { before: Report; after: Report }) {
  const diff = diffReports(before, after);
  const b = countBySeverity(before.findings);
  const a = countBySeverity(after.findings);
  const same = before.readiness === after.readiness;

  return (
    <div className="space-y-6" aria-live="polite">
      <div className="space-y-3 rounded-2xl bg-surface p-5 ring-1 ring-line">
        <h2 className="text-xl font-semibold tracking-[-0.01em]">Что изменилось после ответов</h2>
        <div className="flex flex-wrap items-center gap-3">
          <Verdict report={before} />
          <span aria-hidden>→</span>
          <Verdict report={after} />
          <span className="text-sm text-ink-2">
            {same ? 'Вердикт прежний: остаются подтверждённые находки средней и большей важности.' : 'Вердикт изменился.'}
          </span>
        </div>
        <ul className="tabular grid grid-cols-2 gap-2 sm:grid-cols-4">
          {[...SEVERITY_ORDER].reverse().map((s) => (
            <li key={s} className="rounded-lg bg-sunken px-3 py-2 text-sm">
              <span className="text-muted">{SEVERITY[s].label}</span>
              <p className="font-semibold">
                {b[s]} → {a[s]}
              </p>
            </li>
          ))}
        </ul>
      </div>

      <Section title="Закрытые находки" count={diff.closed.length} tone="bg-ok-bg text-ok-ink">
        {diff.closed.length === 0 ? (
          <p className="text-ink-2">Ни одна находка не закрыта.</p>
        ) : (
          <ul className="space-y-2">{diff.closed.map((f) => <Row key={f.id} finding={f} struck />)}</ul>
        )}
      </Section>

      <Section title="Остались" count={diff.remained.length} tone="bg-sunken text-ink-2">
        <ul className="space-y-2">{diff.remained.map((f) => <Row key={f.id} finding={f} />)}</ul>
      </Section>

      <Section title="Новые находки" count={diff.added.length} tone="bg-sev-high-bg text-sev-high-ink">
        {diff.added.length === 0 ? (
          <p className="text-ink-2">Новых находок после ответов не появилось.</p>
        ) : (
          <ul className="space-y-4">
            {diff.added.map((f) => (
              <li key={f.id}>
                <FindingCard finding={f} docLink />
              </li>
            ))}
          </ul>
        )}
      </Section>
    </div>
  );
}
