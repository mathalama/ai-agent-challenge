import { useRef, useState } from 'react';
import { Link } from 'react-router-dom';
import { ThinkingOrb } from 'thinking-orbs';
import { Button, ButtonLink } from '../components/Button';
import { formatLocation } from '../components/FindingCard';
import { RecheckChanges } from '../components/RecheckChanges';
import { ReportGate } from '../components/ReportGate';
import { SeverityBadge } from '../components/SeverityBadge';
import { CRITERIA, TOPICS } from '../lib/labels';
import { useReportStore } from '../lib/report-store';
import type { Question, Report } from '../lib/types';

const RECHECK_MS = 2600;

function QuestionCard({ q, report }: { q: Question; report: Report }) {
  const live = useReportStore((s) => s.mode === 'live');
  const answer = useReportStore((s) => s.answers[q.id] ?? '');
  const setAnswer = useReportStore((s) => s.setAnswer);
  const clearAnswer = useReportStore((s) => s.clearAnswer);
  const custom = answer !== '' && !q.options.includes(answer);
  const findings = new Map(report.findings.map((f) => [f.id, f]));

  return (
    <article className="space-y-3 rounded-2xl bg-surface p-5 ring-1 ring-line">
      <header className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <SeverityBadge severity={q.priority} />
        <span className="text-xs text-muted">{TOPICS[q.topic] ?? q.topic}</span>
        <span className="tabular text-xs text-muted">{q.id}</span>
        {answer && <span className="ml-auto text-xs font-semibold text-ok-ink">✓ ответ записан</span>}
      </header>
      <h3 className="text-[17px] font-semibold">{q.text}</h3>
      <p className="text-sm text-ink-2">
        <span className="font-semibold text-ink">Почему спрашиваем. </span>
        {q.why}
      </p>

      {q.evidence.length > 0 && (
        <ul className="space-y-1.5">
          {q.evidence.map((ev, i) => (
            <li key={i} className="rounded-lg bg-sunken px-3 py-2 text-sm">
              «{ev.quote}»
              <span className="tabular ml-2 text-xs text-muted">{formatLocation(ev.location)}</span>
            </li>
          ))}
        </ul>
      )}

      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm">
        {q.related.map((r) =>
          r.kind === 'finding' ? (
            <Link key={`f${r.id}`} className="text-accent-ink hover:underline" to={`/document?finding=${encodeURIComponent(r.id)}`}>
              Находка: {findings.get(r.id) ? CRITERIA[findings.get(r.id)!.criterion].label : r.id}
            </Link>
          ) : (
            <Link key={`r${r.id}`} className="text-accent-ink hover:underline" to={`/findings?requirement=${encodeURIComponent(r.id)}`}>
              Требование {r.id}
            </Link>
          ),
        )}
      </div>

      {live ? (
        <ButtonLink to={`/?answer=${encodeURIComponent(q.id)}`}>Ответить в чате</ButtonLink>
      ) : <fieldset className="space-y-2">
        <legend className="sr-only">Ответ на вопрос {q.id}</legend>
        {q.options.map((o) => (
          <label
            key={o}
            className={`flex cursor-pointer items-start gap-2.5 rounded-lg px-3 py-2 text-sm ring-1 transition-colors duration-100 ${
              answer === o ? 'bg-accent-soft ring-accent' : 'ring-line hover:bg-sunken'
            }`}
          >
            <input
              type="radio"
              name={q.id}
              checked={answer === o}
              onChange={() => setAnswer(q.id, o)}
              className="mt-0.5 accent-[var(--color-accent)]"
            />
            {o}
          </label>
        ))}
        <textarea
          aria-label={q.options.length > 0 ? 'Свой вариант ответа' : 'Ответ'}
          placeholder={q.options.length > 0 ? 'Или напишите свой вариант' : 'Ваш ответ'}
          rows={2}
          value={custom ? answer : ''}
          onChange={(e) => (e.target.value === '' ? clearAnswer(q.id) : setAnswer(q.id, e.target.value))}
          className="w-full resize-y rounded-lg bg-surface px-3 py-2 text-sm ring-1 ring-line-strong placeholder:text-muted focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
        />
        {answer && (
          <button type="button" onClick={() => clearAnswer(q.id)} className="cursor-pointer text-xs text-muted hover:underline">
            Очистить ответ
          </button>
        )}
      </fieldset>}
    </article>
  );
}

type Phase =
  | { name: 'idle' }
  | { name: 'running' }
  | { name: 'error'; message: string }
  | { name: 'done'; after: Report };

function QuestionsView({ report }: { report: Report }) {
  const live = useReportStore((s) => s.mode === 'live');
  const answers = useReportStore((s) => s.answers);
  const [phase, setPhase] = useState<Phase>({ name: 'idle' });
  const result = useRef<HTMLDivElement>(null);
  const answered = report.questions.filter((q) => (answers[q.id] ?? '').trim() !== '').length;

  async function recheck() {
    setPhase({ name: 'running' });
    try {
      const [res] = await Promise.all([
        fetch(`${import.meta.env.BASE_URL}mock/report.rechecked.json`),
        new Promise((resolve) => window.setTimeout(resolve, RECHECK_MS)),
      ]);
      if (!res.ok) throw new Error(`Не удалось загрузить повторный отчёт (${res.status})`);
      setPhase({ name: 'done', after: (await res.json()) as Report });
      window.setTimeout(() => result.current?.scrollIntoView({ behavior: 'smooth', block: 'start' }), 50);
    } catch (e) {
      setPhase({ name: 'error', message: e instanceof Error ? e.message : String(e) });
    }
  }

  return (
    <div className="space-y-6">
      <header className="space-y-1">
        <h1 className="text-3xl font-semibold tracking-[-0.02em]">Вопросы к автору</h1>
        <p className="text-ink-2">
          {live ? 'Ответьте на уточняющие вопросы в чате.' : 'Спорные места, которые агент не может решить по тексту. Ответьте и перепроверьте документ.'}
        </p>
      </header>

      {report.questions.length === 0 ? (
        <p className="text-ink-2">Открытых вопросов нет.</p>
      ) : (
        <ul className="space-y-4">
          {report.questions.map((q) => (
            <li key={q.id}>
              <QuestionCard q={q} report={report} />
            </li>
          ))}
        </ul>
      )}

      {!live && <div className="sticky bottom-4 flex flex-wrap items-center gap-3 rounded-2xl bg-surface/95 p-4 ring-1 ring-line-strong backdrop-blur">
        <p className="tabular flex-1 text-sm text-ink-2">
          Отвечено {answered} из {report.questions.length}
        </p>
        <Button disabled={answered === 0 || phase.name === 'running'} onClick={() => void recheck()}>
          Перепроверить с ответами
        </Button>
      </div>}

      {phase.name === 'running' && (
        <div role="status" className="flex items-center gap-4 rounded-2xl bg-surface p-5 ring-1 ring-line">
          <ThinkingOrb state="solving" size={64} theme="light" aria-hidden />
          <div>
            <p className="font-semibold">Перепроверяем с вашими ответами…</p>
            <p className="text-sm text-ink-2">Учитываем ответы и заново сверяем требования.</p>
          </div>
        </div>
      )}
      {phase.name === 'error' && (
        <p role="alert" className="text-sev-critical-ink">{phase.message}</p>
      )}
      {phase.name === 'done' && (
        <div ref={result}>
          <RecheckChanges before={report} after={phase.after} />
        </div>
      )}
    </div>
  );
}

export function QuestionsScreen() {
  return <ReportGate>{(report) => <QuestionsView report={report} />}</ReportGate>;
}
