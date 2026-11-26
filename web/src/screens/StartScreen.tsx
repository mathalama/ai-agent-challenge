import { useRef, useState, type DragEvent } from 'react';
import { Button } from '../components/Button';
import { useReportStore } from '../lib/report-store';

const ACCEPT = '.pdf,.md,.docx';

export function StartScreen() {
  const startRun = useReportStore((s) => s.startRun);
  const input = useRef<HTMLInputElement>(null);
  const [dragging, setDragging] = useState(false);

  // На моке файл не разбирается: любой файл запускает демо-прогон.
  const run = (name: string | null) => {
    startRun(name);
  };

  const onDrop = (e: DragEvent) => {
    e.preventDefault();
    setDragging(false);
    const file = e.dataTransfer.files[0];
    if (file) run(file.name);
  };

  return (
    <div className="chat-inner chat-welcome rise">
      <div className="space-y-3">
        <span className="welcome-mark" aria-hidden><span className="file-symbol" /></span>
        <h1 className="welcome-title">Проверьте требования</h1>
        <p className="max-w-[52ch] text-base text-ink-2">
          Загрузите документ. Агент найдёт дефекты по ISO/IEC/IEEE 29148 и IEEE 830, задаст уточняющие вопросы и скажет,
          можно ли передавать требования в разработку.
        </p>
      </div>

      <div
        onDragOver={(e) => {
          e.preventDefault();
          setDragging(true);
        }}
        onDragLeave={() => setDragging(false)}
        onDrop={onDrop}
        className={`upload-card transition-colors ${
          dragging ? 'scale-[1.01] border-accent bg-accent-soft' : 'border-line-strong bg-surface'
        }`}
      >
        <div>
          <p className="text-base font-semibold">Перетащите файл сюда</p>
          <p className="mt-1 text-sm text-muted">PDF, Markdown или DOCX</p>
        </div>
        <div className="flex flex-wrap items-center gap-3">
          <input
            ref={input}
            type="file"
            accept={ACCEPT}
            className="sr-only"
            tabIndex={-1}
            aria-label="Выбрать файл с требованиями"
            onChange={(e) => {
              const file = e.target.files?.[0];
              if (file) run(file.name);
            }}
          />
          <Button variant="secondary" onClick={() => input.current?.click()}>
            Выбрать файл
          </Button>
          <Button onClick={() => run(null)}>Попробовать на примере</Button>
        </div>
      </div>

      <p className="max-w-[60ch] text-sm text-muted">
        В деморежиме используется готовый отчёт по документу «Уведомление о расходах по карте».
      </p>
    </div>
  );
}
