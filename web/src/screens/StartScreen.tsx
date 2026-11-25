import { useRef, useState, type DragEvent } from 'react';
import { useNavigate } from 'react-router-dom';
import { Button } from '../components/Button';
import { useReportStore } from '../lib/report-store';

const ACCEPT = '.pdf,.md,.docx';

export function StartScreen() {
  const navigate = useNavigate();
  const startRun = useReportStore((s) => s.startRun);
  const input = useRef<HTMLInputElement>(null);
  const [dragging, setDragging] = useState(false);

  // На моке файл не разбирается: любой файл запускает демо-прогон.
  const run = (name: string | null) => {
    startRun(name);
    navigate('/progress');
  };

  const onDrop = (e: DragEvent) => {
    e.preventDefault();
    setDragging(false);
    const file = e.dataTransfer.files[0];
    if (file) run(file.name);
  };

  return (
    <div className="max-w-3xl space-y-8 pt-4 lg:pt-10">
      <div className="space-y-3">
        <h1 className="text-4xl leading-[1.05] font-semibold tracking-[-0.03em] sm:text-5xl">Проверка требований</h1>
        <p className="max-w-[52ch] text-lg text-ink-2">
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
        className={`flex flex-col items-start gap-5 rounded-3xl border-2 border-dashed p-8 transition-[background-color,border-color,transform] duration-200 ease-out sm:p-10 ${
          dragging ? 'scale-[1.01] border-accent bg-accent-soft' : 'border-line-strong bg-surface'
        }`}
      >
        <div>
          <p className="text-xl font-semibold tracking-[-0.01em]">Перетащите файл сюда</p>
          <p className="mt-1 text-ink-2">PDF, Markdown или DOCX</p>
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
        Это демонстрация на готовых данных: файл не отправляется на сервер и не разбирается, результат всегда один и тот
        же, по документу «Уведомление о расходах по карте».
      </p>
    </div>
  );
}
