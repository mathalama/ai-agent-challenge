import { useState } from 'react';
import { Button } from '../components/Button';
import { ReportGate } from '../components/ReportGate';
import { fetchReportExport } from '../lib/chat-api';
import { buildMarkdown, download, reportWithAnswers } from '../lib/export';
import { useReportStore } from '../lib/report-store';
import type { Report } from '../lib/types';

function Content({ report }: { report: Report }) {
  const answers = useReportStore((s) => s.answers);
  const sessionId = useReportStore((s) => s.sessionId);
  const sessionBusy = useReportStore((s) => s.sessionBusy);
  const [downloading, setDownloading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const answered = sessionId
    ? new Set([
      ...Object.values(report.finding_answers ?? {}).flat().map((answer) => answer.question_id),
      ...Object.values(report.requirement_answers ?? {}).flat().map((answer) => answer.question_id),
      ...Object.keys(report.standalone_answers ?? {}),
    ]).size
    : Object.keys(answers).length;
  const stem = `report-${report.passport.date}`;
  const disabled = Boolean(sessionId && sessionBusy) || downloading;

  async function exportReport(format: 'json' | 'md') {
    if (downloading || (sessionId && useReportStore.getState().sessionBusy)) return;
    setError(null);
    setDownloading(true);
    try {
      const content = sessionId
        ? await fetchReportExport(sessionId, format)
        : format === 'json'
          ? JSON.stringify(reportWithAnswers(report, answers), null, 2) + '\n'
          : buildMarkdown(report, answers);
      download(`${stem}.${format}`, content, format === 'json' ? 'application/json' : 'text/markdown');
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : 'Не удалось скачать отчёт.');
    } finally {
      setDownloading(false);
    }
  }

  return (
    <div className="max-w-2xl space-y-8">
      <div className="space-y-1">
        <h1 className="text-3xl font-semibold tracking-[-0.025em]">Экспорт</h1>
        <p className="text-ink-2">
          {report.findings.length} находок, {report.questions.length} вопросов
          {answered > 0 ? `, ответов: ${answered}` : ''}. Ответы на вопросы попадут в файл.
        </p>
      </div>
      <div className="grid gap-4 sm:grid-cols-2">
        <div className="flex flex-col items-start gap-4 rounded-2xl bg-surface p-5 ring-1 ring-line">
          <div>
            <h2 className="font-semibold">JSON</h2>
            <p className="mt-1 text-sm text-ink-2">Отчёт в формате схемы версии {report.schema_version}, для других систем.</p>
          </div>
          <Button disabled={disabled} onClick={() => void exportReport('json')}>Скачать JSON</Button>
        </div>
        <div className="flex flex-col items-start gap-4 rounded-2xl bg-surface p-5 ring-1 ring-line">
          <div>
            <h2 className="font-semibold">Markdown</h2>
            <p className="mt-1 text-sm text-ink-2">Паспорт, таблица требований, находки, вопросы и итог для чтения.</p>
          </div>
          <Button disabled={disabled} onClick={() => void exportReport('md')}>Скачать MD</Button>
        </div>
      </div>
      {sessionId && sessionBusy && <p role="status" className="text-sm text-muted">Скачивание доступно после завершения команды.</p>}
      {error && <p role="alert" className="text-sm text-sev-critical-ink">{error}</p>}
    </div>
  );
}

export function ExportScreen() {
  return <ReportGate>{(report) => <Content report={report} />}</ReportGate>;
}
