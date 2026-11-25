import type { Criterion, Finding, Readiness, Severity } from './types';

const ISO_REQ = 'ISO/IEC/IEEE 29148:2018 § 5.2.5';
const ISO_SET = 'ISO/IEC/IEEE 29148:2018 § 5.2.6';
const IEEE_830 = 'IEEE Std 830-1998 § 4.3';

export const CRITERIA: Record<Criterion, { label: string; standard: string }> = {
  necessary: { label: 'Необходимость', standard: ISO_REQ },
  appropriate: { label: 'Уместность', standard: ISO_REQ },
  unambiguous: { label: 'Однозначность', standard: ISO_REQ },
  complete: { label: 'Полнота требования', standard: ISO_REQ },
  singular: { label: 'Единичность', standard: ISO_REQ },
  feasible: { label: 'Выполнимость требования', standard: ISO_REQ },
  verifiable: { label: 'Проверяемость', standard: ISO_REQ },
  correct: { label: 'Корректность', standard: ISO_REQ },
  conforming: { label: 'Соответствие правилам оформления', standard: ISO_REQ },
  set_complete: { label: 'Полнота набора', standard: ISO_SET },
  consistent: { label: 'Непротиворечивость', standard: ISO_SET },
  set_feasible: { label: 'Выполнимость набора', standard: ISO_SET },
  comprehensible: { label: 'Понятность', standard: ISO_SET },
  able_to_be_validated: { label: 'Возможность валидации', standard: ISO_SET },
  ranked: { label: 'Ранжирование', standard: IEEE_830 },
  modifiable: { label: 'Модифицируемость', standard: IEEE_830 },
  traceable: { label: 'Трассируемость', standard: IEEE_830 },
};

export const CRITERIA_KEYS = Object.keys(CRITERIA) as Criterion[];

/** От низкой к критической: порядок используется для сортировки и «худшей важности». */
export const SEVERITY_ORDER: Severity[] = ['low', 'medium', 'high', 'critical'];

export const SEVERITY: Record<
  Severity,
  { label: string; rank: number; text: string; bg: string; dot: string }
> = {
  low: { label: 'Низкая', rank: 0, text: 'text-sev-low-ink', bg: 'bg-sev-low-bg', dot: 'bg-sev-low' },
  medium: { label: 'Средняя', rank: 1, text: 'text-sev-medium-ink', bg: 'bg-sev-medium-bg', dot: 'bg-sev-medium' },
  high: { label: 'Высокая', rank: 2, text: 'text-sev-high-ink', bg: 'bg-sev-high-bg', dot: 'bg-sev-high' },
  critical: { label: 'Критическая', rank: 3, text: 'text-sev-critical-ink', bg: 'bg-sev-critical-bg', dot: 'bg-sev-critical' },
};

export const READINESS: Record<
  Readiness,
  { label: string; hint: string; text: string; bg: string; dot: string }
> = {
  blocked: {
    label: 'Не готово',
    hint: 'Есть подтверждённые находки средней или большей важности.',
    text: 'text-sev-critical-ink',
    bg: 'bg-sev-critical-bg',
    dot: 'bg-sev-critical',
  },
  checks_incomplete: {
    label: 'Готовность не подтверждена',
    hint: 'Проверка неполная или часть серьёзных находок не подтверждена цитатами.',
    text: 'text-sev-medium-ink',
    bg: 'bg-sev-medium-bg',
    dot: 'bg-sev-medium',
  },
  ready: {
    label: 'Готово',
    hint: 'Подтверждённых дефектов, которые мешают передать документ дальше, нет.',
    text: 'text-ok-ink',
    bg: 'bg-ok-bg',
    dot: 'bg-ok',
  },
};

export const STAGES = [
  { id: 'segment', label: 'Разбор на требования', note: 'Выделяем требования и их источники' },
  { id: 'facts', label: 'Извлечение фактов', note: 'Числа, границы, сущности и сроки' },
  { id: 'rules', label: 'Формальные правила', note: 'Структура, словарь размытых слов' },
  { id: 'requirement_checks', label: 'Проверка требований', note: 'Каждое требование по критериям 29148' },
  { id: 'set_checks', label: 'Проверка набора', note: 'Полнота и выполнимость документа целиком' },
  { id: 'consistency', label: 'Противоречия', note: 'Сравнение фактов между требованиями' },
  { id: 'ambiguity', label: 'Неоднозначности', note: 'Конкурирующие прочтения формулировок' },
  { id: 'verify', label: 'Сверка цитат', note: 'Доказательства против текста документа' },
  { id: 'clarify', label: 'Уточняющие вопросы', note: 'Вопросы к автору по спорным местам' },
] as const;

export type StageId = (typeof STAGES)[number]['id'];

export const STAGE_BY_ID = Object.fromEntries(STAGES.map((s) => [s.id, s])) as Record<
  StageId,
  (typeof STAGES)[number]
>;

export function isStageId(value: string): value is StageId {
  return value in STAGE_BY_ID;
}

/** Стадия, на которой родилась находка: для фильтра «стадия». */
export function findingStage(finding: Finding): StageId {
  const d = finding.detector;
  if (d.kind === 'rule') return 'rules';
  if (d.kind === 'solver') return 'consistency';
  const head = d.source.split(':')[0];
  return isStageId(head) ? head : 'requirement_checks';
}

export const TOPICS: Record<string, string> = {
  calculation: 'Расчёты',
  notifications: 'Уведомления',
  api: 'API',
  security: 'Безопасность',
  other: 'Другое',
};

export const DETECTOR_KIND: Record<string, string> = {
  rule: 'Правило',
  llm: 'Модель',
  solver: 'Решатель',
};
