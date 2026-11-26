import { create } from 'zustand';
import type { Criterion, Finding, Report, Severity } from './types';
import { findingStage, type StageId } from './labels';

export interface StageRecord {
  stage: StageId;
  duration_ms: number;
  succeeded: boolean;
  tokens_in: number;
  tokens_out: number;
}

export type ScopeFilter = 'all' | 'requirement' | 'set';

export interface Filters {
  severities: Severity[];
  criteria: Criterion[];
  stages: StageId[];
  scope: ScopeFilter;
}

export type LoadStatus = 'idle' | 'loading' | 'ready' | 'error';

export async function loadDemoReport(): Promise<{ report: Report; stageTrace: StageRecord[] }> {
  const base = import.meta.env.BASE_URL;
  const [reportRes, stagesRes] = await Promise.all([
    fetch(`${base}mock/report.json`),
    fetch(`${base}mock/stages.json`),
  ]);
  if (!reportRes.ok || !stagesRes.ok) {
    throw new Error(`Не удалось загрузить данные (${reportRes.status}/${stagesRes.status})`);
  }
  const report = (await reportRes.json()) as Report;
  const trace = (await stagesRes.json()) as { stages: StageRecord[] };
  return { report, stageTrace: trace.stages };
}

interface ReportState {
  mode: 'connecting' | 'live' | 'demo' | 'archive';
  sessionId: string | null;
  provider: string | null;
  runId: string | null;
  sessionBusy: boolean;
  status: LoadStatus;
  error: string | null;
  report: Report | null;
  stageTrace: StageRecord[];
  answers: Record<string, string>;
  sourceName: string | null;
  demoPlayed: boolean;

  load: () => Promise<void>;
  reset: () => void;
  startRun: (sourceName: string | null) => void;
  markDemoPlayed: () => void;
  setAnswer: (questionId: string, text: string) => void;
  clearAnswer: (questionId: string) => void;
}

export const useReportStore = create<ReportState>((set, get) => ({
  mode: 'connecting',
  sessionId: null,
  provider: null,
  runId: null,
  sessionBusy: false,
  status: 'idle',
  error: null,
  report: null,
  stageTrace: [],
  answers: {},
  sourceName: null,
  demoPlayed: false,

  load: async () => {
    if (get().status === 'loading' || get().mode === 'connecting') return;
    const { sessionId, runId } = get();
    set({ status: 'loading', error: null });
    try {
      const result = sessionId
        ? { report: await fetchLiveReport(sessionId) }
        : await loadDemoReport();
      if (get().sessionId === sessionId && get().runId === runId && get().status === 'loading') set({ status: 'ready', ...result });
    } catch (e) {
      if (get().sessionId === sessionId && get().runId === runId && get().status === 'loading') {
        set({ status: 'error', error: e instanceof Error ? e.message : String(e) });
      }
    }
  },
  reset: () =>
    set({
      status: 'idle',
      error: null,
      report: null,
      stageTrace: [],
      answers: {},
      sourceName: null,
      runId: null,
      demoPlayed: false,
    }),
  startRun: (sourceName) => {
    get().reset();
    set({ sourceName, runId: crypto.randomUUID?.() ?? Array.from(crypto.getRandomValues(new Uint32Array(4))).join('-') });
  },
  markDemoPlayed: () => set({ demoPlayed: true }),
  setAnswer: (questionId, text) => set((s) => ({ answers: { ...s.answers, [questionId]: text } })),
  clearAnswer: (questionId) =>
    set((s) => {
      const { [questionId]: _removed, ...rest } = s.answers;
      return { answers: rest };
    }),
}));

async function fetchLiveReport(sessionId: string): Promise<Report> {
  const response = await fetch(`/api/sessions/${sessionId}/report`);
  if (!response.ok) throw new Error(`Не удалось загрузить отчёт (${response.status})`);
  return response.json() as Promise<Report>;
}

/** Находки после применения фильтров. Чистая функция, чтобы экран B вызывал её через useMemo. */
export function applyFilters(findings: Finding[], f: Filters): Finding[] {
  return findings.filter(
    (x) =>
      (f.severities.length === 0 || f.severities.includes(x.severity)) &&
      (f.criteria.length === 0 || f.criteria.includes(x.criterion)) &&
      (f.stages.length === 0 || f.stages.includes(findingStage(x))) &&
      (f.scope === 'all' || x.scope.kind === f.scope),
  );
}
