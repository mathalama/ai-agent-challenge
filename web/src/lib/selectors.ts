import type { Finding, Report, Requirement, Severity } from './types';
import { SEVERITY } from './labels';

const overlaps = (a: Requirement['location'], b: Requirement['location']) =>
  a.page === b.page && a.start_line <= b.end_line && b.start_line <= a.end_line;

/**
 * Находки, относящиеся к требованию: по scope или по пересечению цитат с его строками.
 * Так противоречие Ф-3 / Ф-4 видно у обоих требований, хотя у него scope «набор».
 */
export function findingsForRequirement(report: Report, requirement: Requirement): Finding[] {
  return report.findings.filter(
    (f) =>
      (f.scope.kind === 'requirement' && f.scope.id === requirement.id) ||
      (f.scope.kind === 'set' && f.evidence.some((e) => overlaps(e.location, requirement.location))),
  );
}

export function worstSeverity(findings: Finding[]): Severity | null {
  return findings.reduce<Severity | null>(
    (worst, f) => (worst === null || SEVERITY[f.severity].rank > SEVERITY[worst].rank ? f.severity : worst),
    null,
  );
}

export function countBySeverity(findings: Finding[]): Record<Severity, number> {
  const out: Record<Severity, number> = { low: 0, medium: 0, high: 0, critical: 0 };
  for (const f of findings) out[f.severity] += 1;
  return out;
}

/** Главные причины вердикта: критические находки из summary, затем самые важные из остальных. */
export function topReasons(report: Report, limit = 3): Finding[] {
  const byId = new Map(report.findings.map((f) => [f.id, f]));
  const critical = report.summary.critical_findings.flatMap((id) => byId.get(id) ?? []);
  const rest = report.findings
    .filter((f) => !report.summary.critical_findings.includes(f.id) && SEVERITY[f.severity].rank >= SEVERITY.high.rank)
    .sort((a, b) => SEVERITY[b.severity].rank - SEVERITY[a.severity].rank || Number(b.verified) - Number(a.verified));
  return [...critical, ...rest].slice(0, limit);
}
