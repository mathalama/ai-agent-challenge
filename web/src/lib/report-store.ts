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

export const EMPTY_FILTERS: Filters = { severities: [], criteria: [], stages: [], scope: 'all' };

export type LoadStatus = 'idle' | 'loading' | 'ready' | 'error';

/**
 * Единственная точка получения данных. Сейчас читает мок из public/mock,
 * позже сюда подставляется вызов бэкенда.
 */
export async function loadReport(): Promise<{ report: Report; stageTrace: StageRecord[] }> {
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
  status: LoadStatus;
  error: string | null;
  report: Report | null;
  stageTrace: StageRecord[];
  selectedFindingId: string | null;
  selectedRequirementId: string | null;
  filters: Filters;
  answers: Record<string, string>;
  sourceName: string | null;
  demoPlayed: boolean;

  load: () => Promise<void>;
  reset: () => void;
  startRun: (sourceName: string | null) => void;
  markDemoPlayed: () => void;
  selectFinding: (id: string | null) => void;
  selectRequirement: (id: string | null) => void;
  setFilters: (patch: Partial<Filters>) => void;
  toggleFilter: <K extends 'severities' | 'criteria' | 'stages'>(
    key: K,
    value: Filters[K][number],
  ) => void;
  resetFilters: () => void;
  setAnswer: (questionId: string, text: string) => void;
  clearAnswer: (questionId: string) => void;
}

export const useReportStore = create<ReportState>((set, get) => ({
  status: 'idle',
  error: null,
  report: null,
  stageTrace: [],
  selectedFindingId: null,
  selectedRequirementId: null,
  filters: EMPTY_FILTERS,
  answers: {},
  sourceName: null,
  demoPlayed: false,

  load: async () => {
    if (get().status === 'loading') return;
    set({ status: 'loading', error: null });
    try {
      const { report, stageTrace } = await loadReport();
      set({ status: 'ready', report, stageTrace });
    } catch (e) {
      set({ status: 'error', error: e instanceof Error ? e.message : String(e) });
    }
  },
  reset: () =>
    set({
      status: 'idle',
      error: null,
      report: null,
      stageTrace: [],
      selectedFindingId: null,
      selectedRequirementId: null,
      filters: EMPTY_FILTERS,
      answers: {},
      sourceName: null,
      demoPlayed: false,
    }),
  startRun: (sourceName) => {
    get().reset();
    set({ sourceName });
  },
  markDemoPlayed: () => set({ demoPlayed: true }),
  selectFinding: (id) => set({ selectedFindingId: id }),
  selectRequirement: (id) => set({ selectedRequirementId: id }),
  setFilters: (patch) => set((s) => ({ filters: { ...s.filters, ...patch } })),
  toggleFilter: (key, value) =>
    set((s) => {
      const list = s.filters[key] as unknown[];
      const next = list.includes(value) ? list.filter((v) => v !== value) : [...list, value];
      return { filters: { ...s.filters, [key]: next } };
    }),
  resetFilters: () => set({ filters: EMPTY_FILTERS }),
  setAnswer: (questionId, text) => set((s) => ({ answers: { ...s.answers, [questionId]: text } })),
  clearAnswer: (questionId) =>
    set((s) => {
      const { [questionId]: _removed, ...rest } = s.answers;
      return { answers: rest };
    }),
}));

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
