import { useEffect, useRef, useState, type DragEvent, type FormEvent } from 'react';
import { Link, useLocation } from 'react-router-dom';
import { ThinkingOrb } from 'thinking-orbs';
import { Button } from '../components/Button';
import { formatLocation } from '../components/FindingCard';
import { ChatApiError, connectChat, sendChatInput, type ChatEvent, type Progress, type SessionState } from '../lib/chat-api';
import { download } from '../lib/export';
import { READINESS, STAGE_BY_ID, isStageId, type StageId } from '../lib/labels';
import { useReportStore, type StageRecord } from '../lib/report-store';
import type { Evidence, Question, Readiness, ReportSummary } from '../lib/types';
import { StageList } from './ProgressScreen';
import { StartScreen } from './StartScreen';

interface Reason {
  stage: string;
  text: string;
  active: boolean;
}

type Run = {
  kind: 'run';
  title: string;
  stages: StageId[];
  current: StageId | null;
  completed: StageRecord[];
  result?: boolean;
  reasoning: Reason[];
};

type Item =
  | { kind: 'message'; text: string; user?: boolean; error?: boolean; citations?: Evidence[] }
  | { kind: 'question'; question: Question }
  | { kind: 'report'; summary: ReportSummary; readiness: Readiness }
  | { kind: 'reasoning'; value: Reason }
  | Run;

function updateRun(items: Item[], update: (run: Run) => Run): Item[] {
  const index = items.map((item) => item.kind).lastIndexOf('run');
  const latest = items[index];
  if (latest?.kind !== 'run' || latest.result !== undefined) return items;
  return [...items.slice(0, index), update(latest), ...items.slice(index + 1)];
}

function finishReasoning(items: Reason[], stage?: string): Reason[] {
  return items.map((item) => item.active && (!stage || item.stage === stage) ? { ...item, active: false } : item);
}

function appendReasoning(items: Reason[], event: Extract<ChatEvent, { event: 'reasoning' }>): Reason[] {
  const last = items.at(-1);
  if (!event.complete && last?.active && last.stage === event.stage) {
    return [...items.slice(0, -1), { ...last, text: last.text + event.delta }];
  }
  return [...items, { stage: event.stage, text: event.delta, active: !event.complete }];
}

function progress(items: Item[], event: Progress): Item[] {
  switch (event.event) {
    case 'run_started':
      return [...items, { kind: 'run', title: event.document, stages: event.stages.filter(isStageId), current: null, completed: [], reasoning: [] }];
    case 'stage_started':
      return isStageId(event.stage) ? updateRun(items, (run) => ({ ...run, current: event.stage })) : items;
    case 'stage_finished':
      return updateRun(items, (run) => ({
        ...run,
        current: null,
        reasoning: finishReasoning(run.reasoning, event.stage),
        completed: [...run.completed, {
          stage: event.stage, succeeded: event.succeeded, duration_ms: event.duration_ms,
          tokens_in: event.input_tokens, tokens_out: event.output_tokens,
        }],
      }));
    case 'llm_retry':
      return updateRun(items, (run) => ({ ...run, reasoning: finishReasoning(run.reasoning, event.stage) }))
        .map((item) => item.kind === 'reasoning' && item.value.stage === event.stage && item.value.active
          ? { ...item, value: { ...item.value, active: false } } : item);
    case 'run_finished':
      return updateRun(items, (run) => ({ ...run, current: null, result: event.succeeded, reasoning: finishReasoning(run.reasoning) }));
    default:
      return items;
  }
}

export function ChatScreen() {
  const { pathname, search } = useLocation();
  const mode = useReportStore((s) => s.mode);
  const sessionId = useReportStore((s) => s.sessionId);
  const [items, setItems] = useState<Item[]>([]);
  const [state, setState] = useState<SessionState>('empty');
  const [connected, setConnected] = useState(false);
  const [sessionEnded, setSessionEnded] = useState(false);
  const [connectionError, setConnectionError] = useState<string>();
  const busy = useReportStore((s) => s.sessionBusy);
  const [dragging, setDragging] = useState(false);
  const [text, setText] = useState('');
  const uploadInput = useRef<HTMLInputElement>(null);
  const messageInput = useRef<HTMLInputElement>(null);
  const lastLine = useRef<HTMLDivElement>(null);
  const pending = useRef<{ filename?: string } | null>(null);
  const eventSource = useRef<EventSource | null>(null);

  function endSession() {
    eventSource.current?.close();
    pending.current = null;
    setConnected(false);
    useReportStore.setState({ sessionBusy: false });
    setSessionEnded(true);
  }

  useEffect(() => {
    let current = true;
    let reportDirty = false;

    function receive(event: ChatEvent) {
      switch (event.event) {
        case 'message':
          setItems((list) => [...list, { kind: 'message', text: event.text, citations: event.citations }]);
          break;
        case 'question':
          setItems((list) => [...list, { kind: 'question', question: event.question }]);
          break;
        case 'state_changed':
          setState(event.state);
          if (event.state === 'loaded' && pending.current?.filename) {
            useReportStore.getState().startRun(pending.current.filename);
          }
          break;
        case 'progress':
          setItems((list) => progress(list, event.progress));
          if (event.progress.event === 'run_started') {
            useReportStore.setState({ stageTrace: [] });
          } else if (event.progress.event === 'stage_finished') {
            const { stage, succeeded, duration_ms, input_tokens, output_tokens } = event.progress;
            useReportStore.setState(({ stageTrace }) => ({
              stageTrace: [...stageTrace, { stage, succeeded, duration_ms, tokens_in: input_tokens, tokens_out: output_tokens }],
            }));
          }
          break;
        case 'reasoning':
          setItems((list) => {
            if (event.stage !== 'chat') {
              return updateRun(list, (run) => ({ ...run, reasoning: appendReasoning(run.reasoning, event) }));
            }
            const last = list.at(-1);
            const reasons = appendReasoning(last?.kind === 'reasoning' ? [last.value] : [], event);
            return reasons.length === 1 && last?.kind === 'reasoning'
              ? [...list.slice(0, -1), { kind: 'reasoning', value: reasons[0] }]
              : [...list, { kind: 'reasoning', value: reasons[reasons.length - 1] }];
          });
          break;
        case 'report_ready':
          setItems((list) => [...list, { kind: 'report', summary: event.summary, readiness: event.readiness }]);
          reportDirty = true;
          break;
        case 'answers_updated':
          reportDirty = true;
          break;
        case 'export':
          download(`report.${event.format}`, event.content, event.format === 'md' ? 'text/markdown' : 'application/json');
          setItems((list) => [...list, { kind: 'message', text: `Отчёт report.${event.format} скачан.` }]);
          break;
        case 'error':
          setItems((list) => [...list, { kind: 'message', text: event.text, error: true }]);
          break;
      }
    }

    function refreshReport() {
      useReportStore.setState({ status: 'idle' });
      void useReportStore.getState().load();
    }

    void connectChat().then((connection) => {
      if (!current) return;
      if (!connection) {
        useReportStore.setState({ mode: 'demo' });
        return;
      }
      useReportStore.setState({ mode: 'live', sessionId: connection.id });
      receive(connection.greeting);
      const stream = new EventSource(`/api/sessions/${connection.id}/events`);
      eventSource.current = stream;
      stream.onopen = () => setConnected(true);
      stream.onerror = () => {
        if (stream.readyState === EventSource.CLOSED) endSession();
        else setConnected(false);
      };
      stream.addEventListener('expired', endSession);
      stream.addEventListener('history_gap', () => {
        setItems((list) => [...list, { kind: 'message', text: 'Часть истории недоступна. Ниже показаны оставшиеся события.' }]);
      });
      stream.onmessage = (message: MessageEvent<string>) => receive(JSON.parse(message.data) as ChatEvent);
      stream.addEventListener('done', () => {
        if (reportDirty) {
          reportDirty = false;
          refreshReport();
        }
        pending.current = null;
        useReportStore.setState({ sessionBusy: false });
        setItems((list) => list.map((item) => item.kind === 'reasoning' && item.value.active
          ? { ...item, value: { ...item.value, active: false } } : item));
      });
    }).catch((error: Error) => {
      if (current) setConnectionError(error.message);
    });

    return () => {
      current = false;
      eventSource.current?.close();
      eventSource.current = null;
    };
  }, []);

  useEffect(() => {
    if (pathname === '/') lastLine.current?.scrollIntoView({ block: 'nearest' });
    // During streaming the last item can grow without changing the number of items.
  }, [items, pathname]);

  useEffect(() => {
    const id = new URLSearchParams(search).get('answer');
    if (pathname === '/' && id) {
      setText(`/answer ${id} `);
      messageInput.current?.focus();
    }
  }, [pathname, search]);

  async function send(value: string | File) {
    if (!sessionId || !connected || pending.current) return;
    pending.current = typeof value === 'string' ? {} : { filename: value.name };
    useReportStore.setState({ sessionBusy: true });
    setItems((list) => [...list, { kind: 'message', text: typeof value === 'string' ? value : value.name, user: true }]);
    try {
      await sendChatInput(sessionId, value);
    } catch (error) {
      if (error instanceof ChatApiError && error.status === 404) {
        endSession();
        return;
      }
      pending.current = null;
      useReportStore.setState({ sessionBusy: false });
      setItems((list) => [...list, { kind: 'message', text: error instanceof Error ? error.message : String(error), error: true }]);
    }
  }

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (text.trim() && !busy && connected) {
      void send(text.trim());
      setText('');
    }
  }

  function drop(event: DragEvent<HTMLElement>) {
    event.preventDefault();
    setDragging(false);
    const file = event.dataTransfer.files[0];
    if (file) void send(file);
  }

  if (mode === 'demo') return <StartScreen />;
  if (mode === 'connecting') {
    return connectionError
      ? <p role="alert" className="text-sev-critical-ink">{connectionError}</p>
      : <p role="status" className="text-muted">Подключаемся к чату…</p>;
  }

  const disabled = busy || !connected;
  return (
    <section className="mx-auto flex max-w-3xl flex-col gap-5" onDragOver={(event) => {
      event.preventDefault();
      if (!disabled) setDragging(true);
    }} onDrop={drop} onDragLeave={(event) => {
      if (!(event.relatedTarget instanceof Node) || !event.currentTarget.contains(event.relatedTarget)) setDragging(false);
    }}>
      <header>
        <h1 className="text-3xl font-semibold tracking-[-0.025em]">Чат проверки требований</h1>
        {sessionEnded ? (
          <div className="mt-2 flex items-center gap-3">
            <p role="alert" className="text-sm text-muted">Сессия завершилась.</p>
            <Button variant="secondary" onClick={() => window.location.reload()}>Начать новую сессию</Button>
          </div>
        ) : !connected && <p role="status" className="mt-2 text-sm text-muted">Восстанавливаем подключение…</p>}
      </header>

      <div role="log" aria-label="Сообщения" className="max-h-[58dvh] min-h-52 space-y-4 overflow-y-auto rounded-3xl border border-line bg-surface p-4 sm:p-6">
        {items.map((item, index) => <ChatEntry key={index} item={item} onSend={send} disabled={disabled} />)}
        {busy && <div className="flex items-center gap-2 text-sm text-muted" role="status"><ThinkingOrb size={32} state="working" theme="light" aria-hidden /> Агент работает…</div>}
        <div ref={lastLine} />
      </div>

      <form onSubmit={submit} className={`space-y-3 rounded-2xl border p-3 ${dragging ? 'border-accent bg-accent-soft' : 'border-line bg-surface'}`}>
        <input ref={messageInput} aria-label="Сообщение" placeholder={state === 'empty' ? 'Начните с загрузки документа' : 'Напишите сообщение или команду'} value={text} onChange={(event) => setText(event.target.value)} maxLength={4096}
          className="w-full rounded-lg px-3 py-2 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent" />
        <div className="flex flex-wrap items-center gap-2">
          <input ref={uploadInput} type="file" accept=".pdf,.md,.markdown,.docx,.txt,.text" aria-label="Выбрать файл" className="hidden" onChange={(event) => {
            const file = event.target.files?.[0];
            if (file) void send(file);
            event.target.value = '';
          }} />
          <Button disabled={disabled} variant="secondary" onClick={() => uploadInput.current?.click()}>Прикрепить файл</Button>
          {state === 'loaded' && <Button disabled={disabled} variant="secondary" onClick={() => void send('/check')}>Проверить</Button>}
          {state === 'reviewed' && <Button disabled={disabled} variant="secondary" onClick={() => void send('/questions')}>Уточняющие вопросы</Button>}
          <Button type="submit" disabled={disabled || !text.trim()} className="ml-auto">Отправить</Button>
        </div>
      </form>
    </section>
  );
}

function Reasoning({ item }: { item: Reason }) {
  const name = isStageId(item.stage) ? STAGE_BY_ID[item.stage].label : 'Ответ в чате';
  return (
    <details open={item.active} className="rounded-xl border border-line bg-sunken px-4 py-2 text-sm">
      <summary className="cursor-pointer text-muted">Рассуждение · {name}</summary>
      <p className="mt-2 max-h-48 overflow-y-auto whitespace-pre-wrap break-words text-ink-2">{item.text}</p>
    </details>
  );
}

function ChatEntry({ item, onSend, disabled }: { item: Item; onSend: (value: string) => Promise<void>; disabled: boolean }) {
  switch (item.kind) {
    case 'message':
      return (
        <div className={`max-w-[92%] rounded-2xl px-4 py-3 ${item.user ? 'ml-auto w-fit bg-accent-soft' : item.error ? 'bg-sev-critical-bg text-sev-critical-ink' : 'bg-sunken'}`} role={item.error ? 'alert' : undefined}>
          <p className="whitespace-pre-wrap break-words">{item.text}</p>
          {item.citations && item.citations.length > 0 && <ul className="mt-3 space-y-2 text-sm text-ink-2">
            {item.citations.map((source, index) => <li key={index}>«{source.quote}» <span className="text-muted">({formatLocation(source.location)})</span></li>)}
          </ul>}
        </div>
      );
    case 'question':
      return (
        <article className="space-y-2 rounded-2xl border border-line p-4">
          <p className="font-semibold">{item.question.id} · {item.question.text}</p>
          <p className="text-sm text-ink-2">{item.question.why}</p>
          <div className="flex flex-wrap gap-2">
            {item.question.options.map((option) => <Button key={option} disabled={disabled} variant="secondary" onClick={() => void onSend(`/answer ${item.question.id} ${option}`)}>{option}</Button>)}
          </div>
        </article>
      );
    case 'report':
      return (
        <article className="space-y-3 rounded-2xl border border-line p-4">
          <p className={`font-semibold ${READINESS[item.readiness].text}`}>{READINESS[item.readiness].label}</p>
          <p className="text-sm text-ink-2">Требований — {item.summary.requirement_count}, находок — {item.summary.finding_count}.</p>
          <div className="flex flex-wrap gap-x-4 gap-y-2 text-sm text-accent-ink">
            <Link className="hover:underline" to="/summary">Сводка</Link>
            <Link className="hover:underline" to="/findings">Находки</Link>
            <Link className="hover:underline" to="/questions">Вопросы</Link>
            <Link className="hover:underline" to="/export">Экспорт</Link>
          </div>
        </article>
      );
    case 'run':
      return (
        <article className="space-y-2">
          <p className="font-semibold">{item.result === undefined ? 'Проверяем' : item.result ? 'Проверен документ' : 'Проверка прервана'} · {item.title}</p>
          <StageList stages={item.stages} current={item.current} completed={item.completed} />
          {item.reasoning.map((reason, index) => <Reasoning key={index} item={reason} />)}
        </article>
      );
    case 'reasoning':
      return <Reasoning item={item.value} />;
  }
}
