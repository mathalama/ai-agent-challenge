import { useEffect, useState, type FormEvent } from 'react';
import { Button } from './Button';
import {
  NO_JSON_WARNING, PRESETS, getRouterModels, getSettings, presetFor, providerName, putSettings, testSettings,
  type Provider, type ResponseFormat, type RouterModel, type SettingsPatch, type SettingsView,
} from '../lib/settings-api';

type Feedback = { message: string; error?: boolean };
type Draft = Required<Omit<SettingsPatch, 'api_key'>> & { api_key: string };

const FORMATS = [
  { id: 'json_schema', label: 'JSON Schema' },
  { id: 'json_object', label: 'JSON Object' },
  { id: 'none', label: 'Без JSON-режима' },
] as const;

function draftFrom(view: SettingsView, provider: Provider): Draft {
  const profile = provider === 'mock' ? undefined : view[provider];
  return {
    provider,
    model: profile?.model ?? '',
    base_url: profile?.base_url ?? '',
    response_format: profile?.response_format ?? 'json_schema',
    free_only: profile?.free_only ?? true,
    api_key: '',
  };
}

export function ProviderSettings({ onClose, onProvider, fallback }: {
  onClose: () => void;
  onProvider: (name: string) => void;
  fallback: string;
}) {
  const [view, setView] = useState<SettingsView>();
  const [draft, setDraft] = useState<Draft>();
  const [catalog, setCatalog] = useState<RouterModel[]>();
  const [catalogError, setCatalogError] = useState(false);
  const [feedback, setFeedback] = useState<Feedback>();
  const [pending, setPending] = useState<'test' | 'save'>();
  const openai = draft?.provider === 'openai';
  const preset = openai ? presetFor(draft.base_url) : undefined;
  const openrouter = preset?.id === 'openrouter';
  const freeOnly = draft?.free_only ?? true;

  useEffect(() => {
    const request = new AbortController();
    void getSettings(request.signal).then((settings) => {
      setView(settings);
      setDraft(draftFrom(settings, settings.provider));
    }).catch((error: Error) => {
      if (!request.signal.aborted) setFeedback({ message: error.message, error: true });
    });
    return () => request.abort();
  }, []);

  useEffect(() => {
    setCatalog(undefined);
    setCatalogError(false);
    if (!openrouter) return;
    const request = new AbortController();
    void getRouterModels(freeOnly, request.signal).then((models) => {
      if (!request.signal.aborted) setCatalog(models);
    }).catch(() => {
      if (!request.signal.aborted) setCatalogError(true);
    });
    return () => request.abort();
  }, [openrouter, freeOnly]);

  function update(patch: Partial<Draft>) {
    setDraft((current) => current && { ...current, ...patch });
    setFeedback(undefined);
  }

  function chooseModel(model: string) {
    const match = catalog?.find((entry) => entry.id === model);
    update({ model, ...(match && { response_format: match.response_format }) });
  }

  async function submit(action: 'test' | 'save') {
    if (!draft || pending) return;
    const { api_key, ...values } = draft;
    const patch: SettingsPatch = draft.provider === 'mock' ? { provider: draft.provider } : {
      ...values,
      ...(api_key.trim() && { api_key }),
    };
    const match = openrouter && catalog?.find((entry) => entry.id === draft.model);
    if (match) patch.response_format = match.response_format;
    setPending(action);
    setFeedback(undefined);
    try {
      if (action === 'test') {
        setFeedback({ message: (await testSettings(patch)).message });
      } else {
        const settings = await putSettings(patch);
        setView(settings);
        setDraft(draftFrom(settings, settings.provider));
        onProvider(providerName(settings));
        setFeedback({ message: settings.effective_provider === settings.provider
          ? 'Настройки сохранены.'
          : `Настройки сохранены. Сейчас используется ${providerName(settings)} из флага или переменной окружения.` });
      }
    } catch (error) {
      setFeedback({ message: error instanceof Error ? error.message : 'Не удалось выполнить запрос.', error: true });
    } finally {
      setPending(undefined);
    }
  }

  function save(event: FormEvent) {
    event.preventDefault();
    void submit('save');
  }

  const profile = view && draft && draft.provider !== 'mock' ? view[draft.provider] : undefined;
  const selected = openrouter && catalog?.find((model) => model.id === draft?.model);
  const format = selected ? selected.response_format : draft?.response_format;
  return (
    <>
      <header className="settings-header">
        <div><h2>Настройки</h2><p>Активный провайдер · {view ? providerName(view) : fallback}</p></div>
        <button type="button" className="workspace-icon-button" onClick={onClose} aria-label="Закрыть настройки">×</button>
      </header>
      {draft && view && <form className="settings-form" onSubmit={save}>
        <label>Провайдер
          <select value={draft.provider} disabled={Boolean(pending)} onChange={(event) => {
            setDraft(draftFrom(view, event.target.value as Provider));
            setFeedback(undefined);
          }}>
            <option value="mock">Mock</option>
            <option value="gemini">Gemini</option>
            <option value="openai">OpenAI-совместимый</option>
          </select>
        </label>
        {openai && <label>Пресет
          <select value={preset?.id ?? 'custom'} disabled={Boolean(pending)} onChange={(event) => {
            const next = PRESETS.find(({ id }) => id === event.target.value);
            update({ base_url: next?.url ?? 'https://api.openai.com/v1', model: '', api_key: '', response_format: next?.format ?? 'json_schema' });
          }}>
            <option value="custom">Свой адрес</option>
            {PRESETS.map(({ id, name }) => <option key={id} value={id}>{name}</option>)}
          </select>
        </label>}
        {draft.provider !== 'mock' && <>
          <label>Адрес API
            <input type="url" value={draft.base_url} disabled={Boolean(pending)} onChange={(event) => update({ base_url: event.target.value })} />
          </label>
          <label>API-ключ
            <input type="password" value={draft.api_key} disabled={Boolean(pending)} autoComplete="new-password"
              placeholder={profile?.api_key_mask ?? 'Введите ключ'} onChange={(event) => update({ api_key: event.target.value })} />
          </label>
          {openrouter && <>
            <label className="settings-checkbox"><input type="checkbox" checked={freeOnly} disabled={Boolean(pending)}
              onChange={(event) => update({ free_only: event.target.checked })} /> Только бесплатные</label>
            {catalog?.length ? <label>Каталог OpenRouter
              <select value={selected ? selected.id : ''} disabled={Boolean(pending)} onChange={(event) => chooseModel(event.target.value)}>
                <option value="">Выберите модель</option>
                {catalog.map((model) => <option key={model.id} value={model.id}>
                  {model.id} · {model.context_length.toLocaleString('ru')} · {model.response_format === 'none' ? 'без JSON' : model.response_format}
                </option>)}
              </select>
            </label> : <p role="status" className="text-sm text-muted">
              {catalogError || catalog ? 'Каталог недоступен или пуст. Введите ID модели вручную.' : 'Загружаем каталог OpenRouter…'}
            </p>}
          </>}
          <label>{openrouter ? 'ID модели' : 'Модель'}
            <input value={draft.model} disabled={Boolean(pending)} onChange={(event) => chooseModel(event.target.value)} />
          </label>
          {openai && <label>Формат ответа
            <select value={format} disabled={Boolean(pending || selected)} onChange={(event) => update({ response_format: event.target.value as ResponseFormat })}>
              {FORMATS.map((item) => <option key={item.id} value={item.id}>{item.label}</option>)}
            </select>
          </label>}
          {openai && format === 'none' && <p className="settings-warning">{NO_JSON_WARNING}.</p>}
        </>}
        <div className="settings-actions">
          <Button variant="secondary" disabled={Boolean(pending)} onClick={() => void submit('test')}>
            {pending === 'test' ? 'Проверяем…' : 'Проверить соединение'}
          </Button>
          <Button type="submit" disabled={Boolean(pending)}>{pending === 'save' ? 'Сохраняем…' : 'Сохранить'}</Button>
        </div>
      </form>}
      {!draft && !feedback && <p role="status" className="mt-4 text-muted">Загружаем настройки…</p>}
      {feedback && <p role={feedback.error ? 'alert' : 'status'} className={`settings-feedback ${feedback.error ? 'text-sev-critical-ink' : 'text-ok-ink'}`}>{feedback.message}</p>}
    </>
  );
}
