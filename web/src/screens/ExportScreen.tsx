import { Button } from '../components/Button';
import { ReportGate } from '../components/ReportGate';
import { buildMarkdown, download, reportWithAnswers } from '../lib/export';
import { useReportStore } from '../lib/report-store';
import type { Report } from '../lib/types';

function Content({ report }: { report: Report }) {
  const answers = useReportStore((s) => s.answers);
  const answered = Object.keys(answers).length;
  const stem = `report-${report.passport.date}`;

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
          <Button onClick={() => download(`${stem}.json`, JSON.stringify(reportWithAnswers(report, answers), null, 2) + '\n', 'application/json')}>Скачать JSON</Button>
        </div>
        <div className="flex flex-col items-start gap-4 rounded-2xl bg-surface p-5 ring-1 ring-line">
          <div>
            <h2 className="font-semibold">Markdown</h2>
            <p className="mt-1 text-sm text-ink-2">Паспорт, таблица требований, находки, вопросы и итог для чтения.</p>
          </div>
          <Button onClick={() => download(`${stem}.md`, buildMarkdown(report, answers), 'text/markdown')}>Скачать MD</Button>
        </div>
      </div>
    </div>
  );
}

export function ExportScreen() {
  return <ReportGate>{(report) => <Content report={report} />}</ReportGate>;
}
