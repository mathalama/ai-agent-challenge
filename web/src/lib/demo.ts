/** Сколько в среднем длится одна стадия в демо-режиме. Меняется здесь или параметром ?stageMs=… в адресе. */
export const DEMO_STAGE_MS = 2600;
/** Пауза после последней стадии перед переходом на сводку. */
export const DEMO_FINISH_PAUSE_MS = 1000;

export function stageMsFromUrl(search: string): number {
  const raw = new URLSearchParams(search).get('stageMs');
  const value = raw === null ? NaN : Number(raw);
  return Number.isFinite(value) && value >= 0 ? value : DEMO_STAGE_MS;
}

/** Длительность стадии в демо: пропорциональна настоящей, но в пределах 70–130% от среднего. */
export function demoDelay(durationMs: number, maxDurationMs: number, stageMs: number): number {
  const share = maxDurationMs > 0 ? durationMs / maxDurationMs : 0;
  return Math.round(stageMs * (0.7 + 0.6 * share));
}

export const formatSeconds = (ms: number) => `${(ms / 1000).toFixed(1).replace('.', ',')} с`;
export const formatInt = (n: number) => n.toLocaleString('ru-RU');
