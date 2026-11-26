import { useEffect, useMemo, useRef, useState } from 'react';
import { useLocation } from 'react-router-dom';
import { ThinkingOrb, type OrbState } from 'thinking-orbs';
import { ReportMessage } from '../components/ReportMessage';
import { Button } from '../components/Button';
import { demoDelay, DEMO_FINISH_PAUSE_MS, formatInt, formatSeconds, stageMsFromUrl } from '../lib/demo';
import { STAGE_BY_ID, type StageId } from '../lib/labels';
import { useReportStore, type StageRecord } from '../lib/report-store';

const ORB_STATE: Record<StageId, OrbState> = {
  segment: 'working',
  facts: 'searching',
  rules: 'working',
  requirement_checks: 'solving',
  set_checks: 'solving',
  consistency: 'connecting',
  ambiguity: 'searching',
  verify: 'working',
  clarify: 'composing',
};

type Status = 'done' | 'running' | 'pending' | 'error';

function StatusMark({ status }: { status: Status }) {
  if (status === 'error') {
    return <span aria-label="Ошибка" className="grid size-6 place-items-center rounded-full bg-sev-critical-bg text-sev-critical-ink">!</span>;
  }
  if (status === 'done') {
    return (
      <span aria-hidden className="grid size-6 place-items-center rounded-full bg-ok text-white">
        <svg viewBox="0 0 16 16" className="size-3.5" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round">
          <path d="m3.5 8.5 3 3 6-7" />
        </svg>
      </span>
    );
  }
  if (status === 'running') {
    return <span aria-hidden className="pulse-dot grid size-6 place-items-center rounded-full bg-accent"><span className="size-2 rounded-full bg-white" /></span>;
  }
  return <span aria-hidden className="size-6 rounded-full border-2 border-line-strong" />;
}

export function StageList({ stages, current, completed, elapsed }: {
  stages: StageId[];
  current: StageId | null;
  completed: StageRecord[];
  elapsed?: number;
}) {
  return (
    <ol className="divide-y divide-line rounded-2xl border border-line bg-surface">
      {stages.map((stage) => {
        const record = completed.find((item) => item.stage === stage);
        const status = record ? (record.succeeded ? 'done' : 'error') : current === stage ? 'running' : 'pending';
        const info = STAGE_BY_ID[stage];
        return (
          <li key={stage} className="grid grid-cols-[auto_1fr_auto] items-center gap-x-4 gap-y-0.5 px-4 py-3 sm:grid-cols-[auto_1fr_5rem_7rem]">
            <StatusMark status={status} />
            <div className="min-w-0">
              <p className={`font-medium ${status === 'pending' ? 'text-muted' : 'text-ink'}`}>{info.label}</p>
              <p className="hidden text-sm text-muted sm:block">{info.note}</p>
            </div>
            <p className="tabular text-right text-sm text-ink-2">
              {record ? formatSeconds(record.duration_ms) : status === 'running' && elapsed !== undefined ? formatSeconds(elapsed) : ''}
            </p>
            <p className="tabular hidden text-right text-sm text-muted sm:block">
              {record ? (record.tokens_in + record.tokens_out > 0 ? `${formatInt(record.tokens_in)} → ${formatInt(record.tokens_out)}` : 'без токенов') : ''}
            </p>
          </li>
        );
      })}
    </ol>
  );
}

export function DemoProgressScreen() {
  const { search } = useLocation();
  const { status, report, stageTrace, sourceName, demoPlayed, load, markDemoPlayed } = useReportStore();
  const stageMs = stageMsFromUrl(search);

  // Повторный заход после завершённого прогона показывает итог без повторного проигрывания.
  const alreadyPlayed = useRef(demoPlayed);
  const [active, setActive] = useState(0);
  const [elapsed, setElapsed] = useState(0);

  useEffect(() => {
    if (status === 'idle') void load();
  }, [status, load]);

  const total = stageTrace.length;
  const ready = status === 'ready' && total > 0;
  const position = alreadyPlayed.current ? total : active;
  const finished = ready && position >= total;

  const maxDuration = useMemo(() => Math.max(0, ...stageTrace.map((s) => s.duration_ms)), [stageTrace]);

  useEffect(() => {
    if (!ready || finished) return;
    const startedAt = performance.now();
    setElapsed(0);
    const tick = setInterval(() => setElapsed(performance.now() - startedAt), 100);
    const next = setTimeout(() => setActive(position + 1), demoDelay(stageTrace[position].duration_ms, maxDuration, stageMs));
    return () => {
      clearInterval(tick);
      clearTimeout(next);
    };
  }, [ready, finished, position, stageTrace, maxDuration, stageMs]);

  useEffect(() => {
    if (!finished || alreadyPlayed.current) return;
    const go = setTimeout(() => {
      markDemoPlayed();
    }, DEMO_FINISH_PAUSE_MS);
    return () => clearTimeout(go);
  }, [finished, markDemoPlayed]);

  if (status === 'error') {
    return (
      <div role="alert" className="chat-inner space-y-3">
        <h1 className="text-2xl font-semibold">Не удалось начать проверку</h1>
        <p className="text-ink-2">Данные демо не загрузились.</p>
        <Button onClick={() => void load()}>Повторить</Button>
      </div>
    );
  }

  const current = ready && !finished ? stageTrace[position] : null;
  const currentInfo = current ? STAGE_BY_ID[current.stage] : null;
  const totalMs = stageTrace.reduce((s, x) => s + x.duration_ms, 0);
  const totalIn = stageTrace.reduce((s, x) => s + x.tokens_in, 0);
  const totalOut = stageTrace.reduce((s, x) => s + x.tokens_out, 0);

  return (
    <div className="chat-inner space-y-5 rise">
      <div className="flex items-center gap-5 rounded-3xl bg-surface p-6 shadow-[0_1px_2px_rgb(23_24_28/0.06),0_8px_24px_-12px_rgb(23_24_28/0.12)] sm:p-7">
        <div className="grid size-16 shrink-0 place-items-center">
          {current ? (
            <ThinkingOrb state={ORB_STATE[current.stage]} size={64} theme="light" aria-hidden />
          ) : finished ? (
            <StatusMark status="done" />
          ) : null}
        </div>
        <div className="min-w-0" aria-live="polite">
          <h1 className="text-2xl font-semibold tracking-[-0.02em]">
            {finished ? 'Проверка завершена' : (currentInfo?.label ?? 'Запускаем проверку…')}
          </h1>
          <p className="text-ink-2">
            {finished
              ? `Всего ${formatSeconds(totalMs)}, токены ${formatInt(totalIn)} → ${formatInt(totalOut)}`
              : (currentInfo?.note ?? 'Готовим данные')}
          </p>
          {sourceName && <p className="mt-1 truncate text-sm text-muted">Файл: {sourceName}</p>}
        </div>
      </div>

      {demoPlayed && report && <ReportMessage report={report} />}
      {ready && (finished ? (
        <details className="run-details">
          <summary>Стадии проверки</summary>
          <StageList stages={stageTrace.map((stage) => stage.stage)} current={null} completed={stageTrace} />
        </details>
      ) : <StageList stages={stageTrace.map((stage) => stage.stage)} current={current?.stage ?? null} completed={stageTrace.slice(0, position)} elapsed={elapsed} />)}
      {ready && !finished && <Button variant="secondary" onClick={() => setActive(total)}>Пропустить анимацию</Button>}
    </div>
  );
}
