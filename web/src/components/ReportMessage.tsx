import { Link } from 'react-router-dom';
import { CRITERIA, READINESS } from '../lib/labels';
import { topReasons } from '../lib/selectors';
import type { Report } from '../lib/types';
import { formatLocation } from './FindingCard';

export function ReportMessage({ report }: { report: Report }) {
  const { requirement_count, finding_count } = report.summary;
  const reasons = topReasons(report);
  return (
    <article className="chat-report-card">
      <div className="chat-report-heading">
        <span className="welcome-mark" aria-hidden><span className="file-symbol" /></span>
        <div>
          <h2>Отчёт готов</h2>
          <p>{report.passport.document_title}</p>
        </div>
        <Link className="chat-report-open" to="/?tab=summary">Открыть отчёт ↗</Link>
      </div>
      <div className="chat-report-stats">
        <span className={READINESS[report.readiness].text}>{READINESS[report.readiness].label}</span>
        <span>Требований · <b>{requirement_count}</b></span>
        <span>Находок · <b>{finding_count}</b></span>
      </div>
      {report.triage && report.readiness === 'not_requirements' && (
        <p className="text-sm text-ink-2">{report.triage.reason}</p>
      )}
      {reasons.length > 0 && <ul className="chat-finding-links" aria-label="Находки в чате">
        {reasons.map((finding) => <li key={finding.id}>
          <Link to={`/?tab=document&finding=${encodeURIComponent(finding.id)}`}>
            <span className="finding-arrow" aria-hidden>↗</span>
            <span><strong>{CRITERIA[finding.criterion].label}</strong>
              <small>{finding.evidence[0] ? formatLocation(finding.evidence[0].location) : finding.id}</small>
            </span>
          </Link>
        </li>)}
      </ul>}
    </article>
  );
}
