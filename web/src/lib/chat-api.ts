import type { Evidence, Question, Readiness, ReportSummary } from './types';
import type { StageId } from './labels';

export type SessionState = 'empty' | 'loaded' | 'reviewing' | 'reviewed';

export type Progress =
  | { event: 'run_started'; document: string; stages: StageId[] }
  | { event: 'stage_started'; stage: StageId }
  | { event: 'stage_finished'; stage: StageId; succeeded: boolean; duration_ms: number; input_tokens: number; output_tokens: number }
  | { event: 'llm_retry'; stage: string; delay_ms: number }
  | { event: 'run_finished'; succeeded: boolean };

export type ChatEvent =
  | { event: 'message'; role: 'assistant'; text: string; citations?: Evidence[] }
  | { event: 'question'; question: Question }
  | { event: 'state_changed'; state: SessionState }
  | { event: 'progress'; progress: Progress }
  | { event: 'reasoning'; stage: string; delta: string; complete: boolean }
  | { event: 'report_ready'; summary: ReportSummary; readiness: Readiness }
  | { event: 'answers_updated' }
  | { event: 'export'; format: 'md' | 'json'; content: string }
  | { event: 'error'; text: string };

/** Сессия вкладки, к которой страница возвращается после перезагрузки. */
export interface SavedSession {
  sessionId: string;
  runId: string | null;
  sourceName: string | null;
}

interface ChatConnection {
  id: string;
  provider?: string;
  greeting: Extract<ChatEvent, { event: 'message' }>;
  /** События журнала с номером не больше этого уже происходили до перезагрузки. */
  last_event_id?: number;
  resumed?: SavedSession;
}

const SAVED_SESSION = 'rs.chat-session.v1';

function readSavedSession(): SavedSession | null {
  try {
    const value: unknown = JSON.parse(sessionStorage.getItem(SAVED_SESSION) ?? 'null');
    if (!value || typeof value !== 'object') return null;
    const { sessionId, runId, sourceName } = value as Record<string, unknown>;
    if (typeof sessionId !== 'string') return null;
    return {
      sessionId,
      runId: typeof runId === 'string' ? runId : null,
      sourceName: typeof sourceName === 'string' ? sourceName : null,
    };
  } catch {
    return null;
  }
}

export function saveSession(session: SavedSession) {
  try {
    sessionStorage.setItem(SAVED_SESSION, JSON.stringify(session));
  } catch {
    // Без хранилища страница просто начнёт новую сессию после перезагрузки.
  }
}

export class ChatApiError extends Error {
  constructor(message: string, readonly status: number) {
    super(message);
  }
}

async function failure(response: Response): Promise<ChatApiError> {
  const body = await response.json().catch(() => null) as { error?: string } | null;
  return new ChatApiError(body?.error ?? `Ошибка запроса (${response.status})`, response.status);
}

async function createSession(): Promise<ChatConnection | null> {
  const response = await fetch('/api/sessions', { method: 'POST' }).catch(() => null);
  if (!response || response.status === 404 || response.status === 405 ||
      !response.headers.get('content-type')?.includes('application/json')) return null;
  if (!response.ok) throw await failure(response);
  return response.json() as Promise<ChatConnection>;
}

async function resumeSession(saved: SavedSession): Promise<ChatConnection | null> {
  const response = await fetch(`/api/sessions/${encodeURIComponent(saved.sessionId)}`).catch(() => null);
  if (!response?.ok || !response.headers.get('content-type')?.includes('application/json')) return null;
  const connection = await response.json().catch(() => null) as ChatConnection | null;
  return connection?.id ? { ...connection, resumed: saved } : null;
}

// Первое подключение вкладки возвращается к сохранённой сессии, «Новая проверка» создаёт новую.
async function openSession(key: number): Promise<ChatConnection | null> {
  const saved = key === 0 ? readSavedSession() : null;
  return (saved && await resumeSession(saved)) || createSession();
}

// Один запуск при монтировании, в том числе в React Strict Mode.
let connection: { key: number; result: Promise<ChatConnection | null> } | undefined;

export function connectChat(key: number) {
  if (connection?.key !== key) connection = { key, result: openSession(key) };
  return connection.result;
}

export async function sendChatInput(id: string, value: string | File) {
  const base = `/api/sessions/${id}`;
  let response: Response;
  if (typeof value === 'string') {
    response = await fetch(`${base}/messages`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ text: value }),
    });
  } else {
    const form = new FormData();
    form.append('file', value);
    response = await fetch(`${base}/document`, { method: 'POST', body: form });
  }
  if (!response.ok) throw await failure(response);
}

export async function fetchReportExport(id: string, format: 'json' | 'md'): Promise<string> {
  const response = await fetch(`/api/sessions/${id}/report?format=${format}`);
  if (!response.ok) throw await failure(response);
  return response.text();
}
