import { useEffect, useRef, useState, type ReactNode } from 'react';
import { Link, useNavigate, useSearchParams } from 'react-router-dom';
import { useReportStore } from '../lib/report-store';
import { settingsAvailable } from '../lib/settings-api';
import { readHistory, saveHistory, type SavedRun } from '../lib/report-history';
import { ChatScreen } from '../screens/ChatScreen';
import { ProviderSettings } from './ProviderSettings';
import { ReportMessage } from './ReportMessage';
import { SummaryScreen } from '../screens/SummaryScreen';
import { DocumentScreen } from '../screens/DocumentScreen';
import { RequirementsScreen } from '../screens/RequirementsScreen';
import { FindingsScreen } from '../screens/FindingsScreen';
import { QuestionsScreen } from '../screens/QuestionsScreen';
import { ExportScreen } from '../screens/ExportScreen';

const TABS = [
  { id: 'summary', label: 'Сводка', Screen: SummaryScreen },
  { id: 'findings', label: 'Находки', Screen: FindingsScreen },
  { id: 'requirements', label: 'Требования', Screen: RequirementsScreen },
  { id: 'questions', label: 'Вопросы', Screen: QuestionsScreen },
  { id: 'document', label: 'Документ', Screen: DocumentScreen },
  { id: 'export', label: 'Экспорт', Screen: ExportScreen },
] as const;

const historyDate = new Intl.DateTimeFormat('ru', { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });

function useCompactLayout() {
  const [compact, setCompact] = useState(() => window.matchMedia('(max-width: 1099px)').matches);
  useEffect(() => {
    const media = window.matchMedia('(max-width: 1099px)');
    const update = () => setCompact(media.matches);
    media.addEventListener('change', update);
    return () => media.removeEventListener('change', update);
  }, []);
  return compact;
}

function Modal({ label, children, onClose, className }: {
  label: string;
  children: ReactNode;
  onClose: () => void;
  className?: string;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current!;
    dialog.showModal();
    return () => dialog.close();
  }, []);
  return (
    <dialog ref={ref} aria-label={label} className={className} onCancel={(event) => {
      event.preventDefault();
      onClose();
    }}>
      {children}
    </dialog>
  );
}

export function Layout() {
  const navigate = useNavigate();
  const [params] = useSearchParams();
  const compact = useCompactLayout();
  const [history, setHistory] = useState(readHistory);
  const [chatKey, setChatKey] = useState(0);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [canConfigure, setCanConfigure] = useState(false);
  const { mode, report, stageTrace, answers, runId, sourceName, demoPlayed, status, sessionBusy, provider } = useReportStore();
  const tab = TABS.find(({ id }) => id === params.get('tab'));
  const ready = report && (mode !== 'demo' || demoPlayed);
  const active = tab ?? TABS[0];
  const ActiveScreen = active.Screen;
  const showPanel = Boolean(ready && (!compact || tab));
  const providerName = mode === 'demo' ? 'Mock · демо' : provider ?? (mode === 'connecting' ? 'Подключение' : 'Провайдер сервера');

  useEffect(() => {
    if (mode !== 'live' || !report || !runId || status !== 'ready') return;
    setHistory(saveHistory({
      id: runId,
      name: sourceName ?? report.passport.document_title,
      recordedAt: Date.now(),
      report,
      stageTrace,
      answers,
    }));
  }, [report, runId, sourceName, stageTrace, answers, status, mode]);

  useEffect(() => {
    const sync = () => setHistory(readHistory());
    window.addEventListener('storage', sync);
    return () => window.removeEventListener('storage', sync);
  }, []);

  useEffect(() => {
    if (mode !== 'live') return;
    const request = new AbortController();
    void settingsAvailable(request.signal).then((available) => {
      if (!request.signal.aborted) setCanConfigure(available);
    }).catch(() => {
      if (!request.signal.aborted) setCanConfigure(false);
    });
    return () => request.abort();
  }, [mode]);

  function reset(run?: SavedRun) {
    if (sessionBusy) return;
    if (run?.id === runId && mode !== 'archive') {
      setHistoryOpen(false);
      navigate('/?tab=summary');
      return;
    }
    useReportStore.getState().reset();
    useReportStore.setState(run ? {
      mode: 'archive', sessionId: null, report: run.report, status: 'ready',
      runId: run.id, sourceName: run.name, stageTrace: run.stageTrace, answers: run.answers,
    } : { mode: 'connecting', sessionId: null });
    setHistoryOpen(false);
    setChatKey((key) => key + 1);
    navigate(run ? '/?tab=summary' : '/');
  }

  function closeReport() {
    const next = new URLSearchParams();
    if (params.has('stageMs')) next.set('stageMs', params.get('stageMs')!);
    navigate({ pathname: '/', search: next.toString() }, { replace: true });
  }

  const sidebar = (
    <>
      <div className="workspace-brand"><span className="brand-mark" aria-hidden>rs</span><div><strong>Агент проверки</strong><small>Требования</small></div></div>
      <button type="button" className="new-review" disabled={sessionBusy} onClick={() => reset()}><span aria-hidden>＋</span> Новая проверка</button>
      <section className="history-list" aria-label="История проверок">
        <h2>История проверок</h2>
        {history.length === 0 && <p>Завершённые проверки появятся здесь.</p>}
        {history.map((run) => (
          <button key={run.id} type="button" disabled={sessionBusy} aria-current={run.id === runId ? 'true' : undefined} onClick={() => reset(run)}>
            <span className="file-symbol" aria-hidden />
            <span className="history-file"><strong title={run.name}>{run.name}</strong><small>{historyDate.format(run.recordedAt)}</small></span>
          </button>
        ))}
      </section>
      <footer className="workspace-sidebar-footer">
        <p><span className="provider-dot" aria-hidden />{providerName}</p>
        {canConfigure && mode !== 'demo' && <button type="button" onClick={() => { setHistoryOpen(false); setSettingsOpen(true); }}><span aria-hidden>⚙</span> Настройки</button>}
      </footer>
    </>
  );

  const panel = showPanel && (
    <>
      <header className="report-header">
        <div className="report-title"><span className="file-symbol" aria-hidden /> Отчёт о проверке</div>
        {compact && <button type="button" className="workspace-icon-button" onClick={closeReport} aria-label="Закрыть отчёт">×</button>}
      </header>
      <nav aria-label="Разделы отчёта" className="report-tabs">
        {TABS.map(({ id, label }) => (
          <Link key={id} to={`/?tab=${id}`} aria-current={id === active.id ? 'page' : undefined}>{label}</Link>
        ))}
      </nav>
      <div className="report-content" key={`${runId}:${active.id}`}><ActiveScreen /></div>
    </>
  );

  return (
    <div className={`review-workspace${showPanel && !compact ? ' has-report' : ''}`}>
      <aside className="workspace-sidebar" aria-label="Проверки">{!compact && sidebar}</aside>
      <main className="workspace-chat">
        <header className="chat-header">
          {compact && <button type="button" className="workspace-icon-button" onClick={() => setHistoryOpen(true)} aria-label="Открыть историю">☰</button>}
          <div className="chat-title"><strong>{sourceName ?? 'Проверка требований'}</strong><small>{mode === 'archive' ? 'Сохранённый отчёт' : 'Рабочая область'}</small></div>
          {ready && compact && <Link to="/?tab=summary" className="mobile-report-link">Отчёт ↗</Link>}
        </header>
        {mode === 'archive' && report
          ? <div className="chat-inner archive-chat"><ReportMessage report={report} /></div>
          : <ChatScreen key={chatKey} sessionKey={chatKey} onRestart={() => reset()} />}
      </main>
      {showPanel && (compact
        ? <Modal label="Отчёт о проверке" onClose={closeReport} className="report-panel report-drawer">{panel}</Modal>
        : <aside className="report-panel" aria-label="Отчёт о проверке">{panel}</aside>)}
      {historyOpen && compact && <Modal label="История проверок" className="history-drawer" onClose={() => setHistoryOpen(false)}>
        <button type="button" className="workspace-icon-button history-close" onClick={() => setHistoryOpen(false)} aria-label="Закрыть историю">×</button>
        {sidebar}
      </Modal>}
      {settingsOpen && <Modal label="Настройки" onClose={() => setSettingsOpen(false)} className="workspace-settings">
        <ProviderSettings fallback={providerName} onClose={() => setSettingsOpen(false)}
          onProvider={(name) => useReportStore.setState({ provider: name })} />
      </Modal>}
    </div>
  );
}
