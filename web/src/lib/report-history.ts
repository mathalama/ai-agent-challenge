import type { StageRecord } from './report-store';
import type { Report } from './types';

export interface SavedRun {
  id: string;
  name: string;
  recordedAt: number;
  report: Report;
  stageTrace: StageRecord[];
  answers: Record<string, string>;
}

const KEY = 'rs.review-history.v1';
const LIMIT = 20;

export function readHistory(): SavedRun[] {
  try {
    const entries: unknown = JSON.parse(localStorage.getItem(KEY) ?? '[]');
    if (!Array.isArray(entries)) return [];
    return entries.filter((run): run is SavedRun =>
      typeof run?.id === 'string' && typeof run.name === 'string' &&
      typeof run.recordedAt === 'number' && run.report?.passport &&
      Array.isArray(run.report.findings) && Array.isArray(run.report.questions) &&
      Array.isArray(run.stageTrace) && run.answers && typeof run.answers === 'object',
    );
  } catch {
    return [];
  }
}

export function saveHistory(run: SavedRun): SavedRun[] {
  let history = [run, ...readHistory().filter((entry) => entry.id !== run.id)].slice(0, LIMIT);
  while (history.length > 0) {
    try {
      localStorage.setItem(KEY, JSON.stringify(history));
      return history;
    } catch {
      history = history.slice(0, -1);
    }
  }
  return readHistory();
}
