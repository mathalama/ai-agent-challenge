export type Provider = 'mock' | 'gemini' | 'openai';
export type ResponseFormat = 'json_schema' | 'json_object' | 'none';

export interface Profile {
  api_key_mask: string | null;
  model: string;
  base_url: string;
  response_format: ResponseFormat;
  free_only: boolean;
  env_overrides: string[];
}

export interface SettingsView {
  provider: Provider;
  effective_provider: Provider;
  gemini: Profile;
  openai: Profile;
}

export interface SettingsPatch {
  provider: Provider;
  model?: string;
  base_url?: string;
  api_key?: string;
  response_format?: ResponseFormat;
  free_only?: boolean;
}

export interface RouterModel {
  id: string;
  context_length: number;
  response_format: ResponseFormat;
}

export const OPENROUTER_URL = 'https://openrouter.ai/api/v1';
export const NO_JSON_WARNING = 'Модель не поддерживает JSON-режим, отчёт может не собраться';
export const PRESETS = [
  { id: 'openrouter', name: 'OpenRouter', url: OPENROUTER_URL, format: 'json_schema' },
  { id: 'groq', name: 'Groq', url: 'https://api.groq.com/openai/v1', format: 'json_object' },
  { id: 'deepseek', name: 'DeepSeek', url: 'https://api.deepseek.com/v1', format: 'json_object' },
  { id: 'ollama', name: 'Ollama', url: 'http://localhost:11434/v1', format: 'json_object' },
] as const;

export function presetFor(url: string) {
  return PRESETS.find((preset) => preset.url === url.trim().replace(/\/+$/, ''));
}

export function providerName(view: SettingsView) {
  switch (view.effective_provider) {
    case 'mock': return 'Mock';
    case 'gemini': return 'Gemini';
    case 'openai': return presetFor(view.openai.base_url)?.name ?? 'OpenAI-совместимый';
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`/api/settings${path}`, init);
  if (!response.ok || !response.headers.get('content-type')?.includes('application/json')) {
    const body = await response.json().catch(() => null) as { error?: string } | null;
    throw new Error(body?.error ?? 'Настройки сервера недоступны.');
  }
  return response.json() as Promise<T>;
}

function write<T>(path: string, method: 'POST' | 'PUT', patch: SettingsPatch) {
  return request<T>(path, { method, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(patch) });
}

export async function settingsAvailable(signal: AbortSignal) {
  const response = await fetch('/api/settings', { method: 'HEAD', signal });
  return response.ok && Boolean(response.headers.get('content-type')?.includes('application/json'));
}

export const getSettings = (signal: AbortSignal) => request<SettingsView>('', { signal });
export const putSettings = (patch: SettingsPatch) => write<SettingsView>('', 'PUT', patch);
export const testSettings = (patch: SettingsPatch) => write<{ message: string }>('/test', 'POST', patch);
export const getRouterModels = (freeOnly: boolean, signal: AbortSignal) =>
  request<RouterModel[]>(`/openrouter/models?free_only=${freeOnly}`, { signal });
