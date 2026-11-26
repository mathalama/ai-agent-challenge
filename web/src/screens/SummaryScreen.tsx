import { Link } from 'react-router-dom';
import { BorderBeam } from 'border-beam';
import { ButtonLink } from '../components/Button';
import { ReportGate } from '../components/ReportGate';
import { SeverityBadge } from '../components/SeverityBadge';
import { CRITERIA, READINESS, SEVERITY, SEVERITY_ORDER } from '../lib/labels';
import { countBySeverity, topReasons } from '../lib/selectors';
import type { Finding, Report } from '../lib/types';

function Reason({ finding }: { finding: Finding }) {
  const card = (
    <div className="space-y-3 rounded-2xl bg-surface p-5 ring-1 ring-line">
      <div className="flex flex-wrap items-center gap-2">
        <SeverityBadge severity={finding.severity} />
        <span className="text-sm font-medium text-ink-2">{CRITERIA[finding.criterion].label}</span>
        {!finding.verified && <span className="text-xs text-muted">цитата не подтверждена</span>}
      </div>
      <p className="max-w-[68ch] text-[15px] leading-relaxed">{finding.explanation}</p>
      <div className="flex flex-wrap gap-x-5 gap-y-1 text-sm font-medium">
        <Link className="text-accent-ink hover:underline" to={`/?tab=findings&finding=${encodeURIComponent(finding.id)}`}>Разобрать находку</Link>
        <Link className="text-accent-ink hover:underline" to={`/?tab=document&finding=${encodeURIComponent(finding.id)}`}>Показать в документе</Link>
      </div>
    </div>
  );
  return finding.severity === 'critical' ? (
    <BorderBeam size="md" colorVariant="sunset" theme="light" strength={0.85}>
      {card}
    </BorderBeam>
  ) : (
    card
  );
}

function Tile({ title, children, className = "" }: { title: string; children: React.ReactNode; className?: string }) {
  return (
    <div className={`rounded-xl bg-surface p-3.5 ring-1 ring-line ${className}`}>
      <p className="text-sm text-muted">{title}</p>
      <div className="mt-1.5">{children}</div>
    </div>
  );
}

function Content({ report }: { report: Report }) {
  const verdict = READINESS[report.readiness];
  const reasons = topReasons(report);
  const counts = countBySeverity(report.findings);
  const { requirement_count, finding_count, verified_count } = report.summary;
  const share = finding_count === 0 ? 0 : Math.round((verified_count / finding_count) * 100);
  const { passport } = report;

  return (
    <div className="space-y-7">
      <section aria-labelledby="verdict" className="space-y-4">
        <p className="text-sm text-muted">{passport.document_title}</p>
        <h1 id="verdict" className="flex items-center gap-4 text-3xl leading-tight font-semibold tracking-[-0.025em]">
          <span aria-hidden className={`mt-1 size-4 shrink-0 rounded-full sm:size-5 ${verdict.dot}`} />
          {verdict.label}
        </h1>
        <p className="max-w-[56ch] text-sm text-ink-2">{verdict.hint}</p>
      </section>

      <section aria-label="Итоги проверки" className="grid grid-cols-2 gap-3">
        <Tile title="Требований найдено">
          <p className="tabular text-3xl font-semibold tracking-[-0.02em]">{requirement_count}</p>
        </Tile>
        <Tile title="Находки" className="row-span-2">
          <p className="tabular mb-2 text-3xl font-semibold tracking-[-0.02em]">{finding_count}</p>
          <ul className="space-y-1">
            {[...SEVERITY_ORDER].reverse().map((sev) => (
              <li key={sev} className="flex items-center justify-between gap-3 text-sm">
                <span className="flex items-center gap-2 text-ink-2">
                  <span aria-hidden className={`size-2 rounded-full ${SEVERITY[sev].dot}`} />
                  {SEVERITY[sev].label}
                </span>
                <span className="tabular font-semibold">{counts[sev]}</span>
              </li>
            ))}
          </ul>
        </Tile>
        <Tile title="Цитаты подтверждены">
          <p className="tabular text-3xl font-semibold tracking-[-0.02em]">{share}%</p>
          <div className="mt-3 h-1.5 overflow-hidden rounded-full bg-sunken" role="img" aria-label={`${verified_count} из ${finding_count}`}>
            <div className="h-full rounded-full bg-ok" style={{ width: `${share}%` }} />
          </div>
          <p className="tabular mt-2 text-sm text-muted">{verified_count} из {finding_count} находок</p>
        </Tile>
      </section>

      {reasons.length > 0 && (
        <section aria-labelledby="reasons" className="space-y-4">
          <h2 id="reasons" className="text-xl font-semibold tracking-[-0.015em]">Почему такой вердикт</h2>
          <div className="space-y-4">
            {reasons.map((f) => (
              <Reason key={f.id} finding={f} />
            ))}
          </div>
        </section>
      )}

      <section aria-labelledby="passport" className="space-y-3">
        <h2 id="passport" className="text-xl font-semibold tracking-[-0.015em]">Паспорт прогона</h2>
        <dl className="grid grid-cols-[max-content_1fr] gap-x-8 gap-y-2 text-sm">
          {[
            ['Документ', passport.document_title],
            ['Дата', passport.date],
            ['Модель', passport.model],
            ['Версия агента', passport.agent_version],
          ].map(([k, v]) => (
            <div key={k} className="contents">
              <dt className="text-muted">{k}</dt>
              <dd className="font-medium">{v}</dd>
            </div>
          ))}
        </dl>
      </section>

      <div className="flex flex-wrap gap-3">
        <ButtonLink to="/?tab=findings" variant="primary">Открыть находки</ButtonLink>
        {report.questions.length > 0 && <ButtonLink to="/?tab=questions">Ответить на вопросы ({report.questions.length})</ButtonLink>}
        <ButtonLink to="/?tab=export">Экспорт</ButtonLink>
      </div>
    </div>
  );
}

export function SummaryScreen() {
  return <ReportGate>{(report) => <Content report={report} />}</ReportGate>;
}
