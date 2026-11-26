import { CRITERIA, DETECTOR_KIND, READINESS, SEVERITY, SEVERITY_ORDER, TOPICS } from './labels';
import { findingsForRequirement, worstSeverity } from './selectors';
import type { Answer, Evidence, Report } from './types';

/** Отчёт с ответами по связанным находкам и требованиям. */
export function reportWithAnswers(report: Report, answers: Record<string, string>): Report {
  const finding_answers: Record<string, Answer[]> = { ...(report.finding_answers ?? {}) };
  const requirement_answers: Record<string, Answer[]> = { ...(report.requirement_answers ?? {}) };
  const standalone_answers = { ...(report.standalone_answers ?? {}) };
  for (const q of report.questions) {
    const text = answers[q.id];
    if (!text) continue;
    if (q.related.length === 0) standalone_answers[q.id] = text;
    for (const rel of q.related) {
      const related = rel.kind === 'finding' ? finding_answers : requirement_answers;
      const list = (related[rel.id] ?? []).filter((a) => a.question_id !== q.id);
      related[rel.id] = [...list, { question_id: q.id, text }];
    }
  }
  return {
    ...report,
    ...(Object.keys(finding_answers).length > 0 && { finding_answers }),
    ...(Object.keys(requirement_answers).length > 0 && { requirement_answers }),
    ...(Object.keys(standalone_answers).length > 0 && { standalone_answers }),
  };
}

const cell = (s: string) => s.replace(/\|/g, '\\|').replace(/\s*\n\s*/g, ' ');
const lines = (e: Evidence) => {
  const { start_line: a, end_line: b, page } = e.location;
  const range = a === b ? `строка ${a}` : `строки ${a}–${b}`;
  return page === null ? range : `стр. ${page}, ${range}`;
};
const quote = (e: Evidence) => `> ${e.quote.replace(/\n/g, '\n> ')}\n> — ${lines(e)}`;

export function buildMarkdown(report: Report, answers: Record<string, string>): string {
  const { passport, summary } = report;
  const out: string[] = [];

  out.push(`# Отчёт о проверке требований`, '');
  out.push('## Паспорт прогона', '');
  out.push(
    `- Документ: ${passport.document_title}`,
    `- Дата: ${passport.date}`,
    `- Модель: ${passport.model}`,
    `- Версия агента: ${passport.agent_version}`,
    `- Версия схемы отчёта: ${report.schema_version}`,
    '',
  );

  out.push('## Требования', '');
  out.push('| ID | ID в документе | Формулировка | Находок | Худшая важность |', '| --- | --- | --- | --- | --- |');
  for (const r of report.requirements) {
    const found = findingsForRequirement(report, r);
    const worst = worstSeverity(found);
    out.push(`| ${r.id} | ${r.source_id ?? '—'} | ${cell(r.text)} | ${found.length} | ${worst ? SEVERITY[worst].label : '—'} |`);
  }
  out.push('');

  out.push('## Находки', '');
  const sorted = [...report.findings].sort(
    (a, b) => SEVERITY_ORDER.indexOf(b.severity) - SEVERITY_ORDER.indexOf(a.severity),
  );
  for (const f of sorted) {
    out.push(`### ${f.id}. ${CRITERIA[f.criterion].label}`, '');
    out.push(
      `- Важность: ${SEVERITY[f.severity].label}`,
      `- Область: ${f.scope.kind === 'set' ? 'набор требований' : `требование ${f.scope.id}`}`,
      `- Критерий: ${CRITERIA[f.criterion].label} (${CRITERIA[f.criterion].standard})`,
      `- Источник: ${DETECTOR_KIND[f.detector.kind]}`,
      `- Цитаты: ${f.verified ? 'подтверждены текстом документа' : 'не подтверждены'}`,
      '',
      f.explanation,
      '',
    );
    for (const e of f.evidence) out.push(quote(e), '');
    if (f.suggestion) out.push(`**Что сделать:** ${f.suggestion}`, '');
  }

  out.push('## Вопросы', '');
  if (report.questions.length === 0) out.push('Открытых вопросов нет.', '');
  for (const q of report.questions) {
    out.push(`### ${q.text}`, '', `Приоритет: ${SEVERITY[q.priority].label}. Тема: ${TOPICS[q.topic] ?? q.topic}.`, '', q.why, '');
    if (q.options.length) out.push(...q.options.map((o) => `- ${o}`), '');
    out.push(`Ответ: ${answers[q.id] ?? 'не дан'}`, '');
  }

  const r = READINESS[report.readiness];
  out.push('## Итог', '');
  if (report.readiness === 'not_requirements' && report.triage) {
    out.push(`**${r.label}.**`, '', `Причина: ${report.triage.reason}`, '', quote(report.triage.evidence), '');
  } else {
    out.push(
      `**${r.label}.** ${r.hint}`,
      '',
    );
    if (report.triage && report.triage.kind === 'stub') {
      out.push(`В документе нет содержательных требований: ${report.triage.reason}`, '', quote(report.triage.evidence), '');
    }
    out.push(
      `Требований: ${summary.requirement_count}. Находок: ${summary.finding_count}, из них с подтверждёнными цитатами: ${summary.verified_count}.`,
      '',
    );
  }
  return out.join('\n');
}

export function download(filename: string, content: string, mime: string) {
  const url = URL.createObjectURL(new Blob([content], { type: `${mime};charset=utf-8` }));
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
