import { useEffect } from 'react';
import { NavLink, Outlet, useLocation } from 'react-router-dom';
import { useReportStore } from '../lib/report-store';

const NAV = [
  { to: '/', label: 'Старт' },
  { to: '/progress', label: 'Прогресс' },
  { to: '/summary', label: 'Сводка' },
  { to: '/document', label: 'Документ' },
  { to: '/requirements', label: 'Требования' },
  { to: '/findings', label: 'Находки' },
  { to: '/questions', label: 'Вопросы' },
  { to: '/export', label: 'Экспорт' },
] as const;

/** Экраны, которые сами запускают прогон. На остальных данные подгружаются по прямой ссылке. */
const SELF_LOADING = new Set(['/', '/progress']);

export function Layout() {
  const { pathname } = useLocation();
  const status = useReportStore((s) => s.status);
  const report = useReportStore((s) => s.report);
  const load = useReportStore((s) => s.load);

  useEffect(() => {
    if (status === 'idle' && !SELF_LOADING.has(pathname)) void load();
  }, [pathname, status, load]);

  const counts: Record<string, number | undefined> = {
    '/findings': report?.findings.length,
    '/questions': report?.questions.length,
    '/requirements': report?.requirements.length,
  };

  return (
    <div className="min-h-dvh lg:grid lg:grid-cols-[15rem_1fr]">
      <header className="border-b border-line bg-ground/90 backdrop-blur lg:sticky lg:top-0 lg:h-dvh lg:border-r lg:border-b-0">
        <div className="flex items-center gap-3 px-5 py-4 lg:block lg:px-6 lg:pt-7 lg:pb-5">
          <p className="text-[15px] leading-tight font-semibold tracking-[-0.01em]">Агент проверки требований</p>
          <p className="hidden text-xs text-muted lg:mt-1 lg:block">ISO/IEC/IEEE 29148 · IEEE 830</p>
        </div>
        <nav aria-label="Разделы" className="flex gap-1 overflow-x-auto px-3 pb-3 lg:flex-col lg:px-3 lg:pb-0">
          {NAV.map((item) => (
            <NavLink
              key={item.to}
              to={item.to}
              end={item.to === '/'}
              className={({ isActive }) =>
                `flex items-center justify-between gap-3 rounded-lg px-3 py-2 text-sm whitespace-nowrap transition-colors duration-150 ${
                  isActive ? 'bg-surface font-semibold text-ink shadow-[0_1px_2px_rgb(23_24_28/0.08)]' : 'text-ink-2 hover:bg-sunken'
                }`
              }
            >
              {item.label}
              {counts[item.to] !== undefined && (
                <span className="tabular rounded-full bg-sunken px-1.5 text-xs text-muted">{counts[item.to]}</span>
              )}
            </NavLink>
          ))}
        </nav>
      </header>
      <main className="min-w-0 px-5 py-8 sm:px-8 lg:px-12 lg:py-10">
        <div key={pathname} className="rise mx-auto max-w-5xl">
          <Outlet />
        </div>
      </main>
    </div>
  );
}
