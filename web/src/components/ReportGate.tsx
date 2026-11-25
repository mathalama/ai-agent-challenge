import type { ReactNode } from 'react';
import type { Report } from '../lib/types';
import { useReportStore } from '../lib/report-store';
import { Button, ButtonLink } from './Button';

/** Показывает загрузку, ошибку или пустое состояние; иначе отдаёт готовый отчёт. */
export function ReportGate({ children }: { children: (report: Report) => ReactNode }) {
  const { status, report, error, load } = useReportStore();

  if (status === 'error') {
    return (
      <div role="alert" className="max-w-md space-y-3">
        <h2 className="text-lg font-semibold">Не удалось загрузить отчёт</h2>
        <p className="text-ink-2">{error}</p>
        <Button onClick={() => void load()}>Повторить</Button>
      </div>
    );
  }
  if (!report) {
    return status === 'loading' ? (
      <p role="status" className="text-muted">Загружаем отчёт…</p>
    ) : (
      <div className="max-w-md space-y-3">
        <h2 className="text-lg font-semibold">Отчёта пока нет</h2>
        <p className="text-ink-2">Загрузите документ на старте, чтобы получить результат проверки.</p>
        <ButtonLink to="/" variant="primary">В чат</ButtonLink>
      </div>
    );
  }
  return <>{children(report)}</>;
}
