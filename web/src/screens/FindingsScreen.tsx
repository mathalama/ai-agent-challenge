import { useEffect, useMemo, useRef } from 'react';
import { Link, useSearchParams } from 'react-router-dom';
import { FindingCard } from '../components/FindingCard';
import { ReportGate } from '../components/ReportGate';
import { Button } from '../components/Button';
import {
  CRITERIA,
  CRITERIA_KEYS,
  SEVERITY,
  SEVERITY_ORDER,
  STAGES,
  findingStage,
  isStageId,
} from '../lib/labels';
import { applyFilters, useReportStore, type Filters, type ScopeFilter } from '../lib/report-store';
import { findingsForRequirement } from '../lib/selectors';
import type { Criterion, Report, Severity } from '../lib/types';

const SCOPES: { value: ScopeFilter; label: string }[] = [
  { value: 'all', label: 'Все' },
  { value: 'requirement', label: 'Требование' },
  { value: 'set', label: 'Весь набор' },
];

const FILTER_KEYS = ['severity', 'criterion', 'stage', 'scope', 'requirement', 'finding'];

const csv = (p: string | null) => (p ? p.split(',').filter(Boolean) : []);

/** Адрес → стор. Выполняется один раз при открытии экрана, если в адресе есть параметры. */
function readParams(p: URLSearchParams) {
  const filters: Filters = {
    severities: csv(p.get('severity')).filter((v): v is Severity => v in SEVERITY),
    criteria: csv(p.get('criterion')).filter((v): v is Criterion => v in CRITERIA),
    stages: csv(p.get('stage')).filter(isStageId),
    scope: SCOPES.some((s) => s.value === p.get('scope')) ? (p.get('scope') as ScopeFilter) : 'all',
  };
  return { filters, requirement: p.get('requirement'), finding: p.get('finding') };
}

function Chip({
  active,
  onClick,
  children,
  count,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
  count?: number;
}) {
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onClick}
      className={`inline-flex cursor-pointer items-center gap-1.5 rounded-full px-3 py-1 text-sm transition-colors duration-100 ${
        active ? 'bg-ink text-white' : 'bg-surface text-ink-2 ring-1 ring-line-strong hover:bg-sunken'
      } ${count === 0 && !active ? 'opacity-50' : ''}`}
    >
      {children}
      {count !== undefined && <span className={`tabular text-xs ${active ? 'text-white/70' : 'text-muted'}`}>{count}</span>}
    </button>
  );
}

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <fieldset className="space-y-1.5">
      <legend className="text-xs font-semibold tracking-wide text-muted uppercase">{title}</legend>
      <div className="flex flex-wrap gap-1.5">{children}</div>
    </fieldset>
  );
}

function FindingsView({ report }: { report: Report }) {
  const { filters, selectedRequirementId, selectedFindingId } = useReportStore();
  const { toggleFilter, setFilters, resetFilters, selectRequirement, selectFinding } = useReportStore.getState();
  const [params, setParams] = useSearchParams();
  const ready = useRef(false);
  const scrolled = useRef<string | null>(null);

  // Адрес ↔ стор. При входе адрес главнее, если в нём есть параметры; затем адрес следует за стором.
  useEffect(() => {
    if (!ready.current) {
      ready.current = true;
      if (FILTER_KEYS.some((k) => params.has(k))) {
        const { filters: f, requirement, finding } = readParams(params);
        setFilters(f);
        selectRequirement(requirement && report.requirements.some((r) => r.id === requirement) ? requirement : null);
        selectFinding(finding && report.findings.some((x) => x.id === finding) ? finding : null);
        return;
      }
    }
    const next = new URLSearchParams();
    if (filters.severities.length) next.set('severity', filters.severities.join(','));
    if (filters.criteria.length) next.set('criterion', filters.criteria.join(','));
    if (filters.stages.length) next.set('stage', filters.stages.join(','));
    if (filters.scope !== 'all') next.set('scope', filters.scope);
    if (selectedRequirementId) next.set('requirement', selectedRequirementId);
    if (selectedFindingId) next.set('finding', selectedFindingId);
    if (next.toString() !== params.toString()) setParams(next, { replace: true });
  }, [filters, selectedRequirementId, selectedFindingId, params, report, setParams, setFilters, selectRequirement, selectFinding]);

  const requirement = report.requirements.find((r) => r.id === selectedRequirementId) ?? null;

  const base = useMemo(
    () => (requirement ? findingsForRequirement(report, requirement) : report.findings),
    [report, requirement],
  );
  const visible = useMemo(
    () =>
      applyFilters(base, filters).sort(
        (a, b) =>
          SEVERITY[b.severity].rank - SEVERITY[a.severity].rank || Number(b.verified) - Number(a.verified),
      ),
    [base, filters],
  );

  const count = <T,>(pick: (f: Report['findings'][number]) => T, value: T) =>
    base.filter((f) => pick(f) === value).length;

  useEffect(() => {
    if (selectedFindingId && scrolled.current !== selectedFindingId) {
      const el = document.getElementById(`finding-${selectedFindingId}`);
      if (el) {
        scrolled.current = selectedFindingId;
        el.scrollIntoView({ block: 'center' });
      }
    }
  }, [selectedFindingId, visible]);

  const dirty =
    filters.severities.length > 0 || filters.criteria.length > 0 || filters.stages.length > 0 || filters.scope !== 'all';
  const anyFilter = dirty || requirement !== null;

  return (
    <div className="space-y-6">
      <header className="space-y-1">
        <h1 className="text-3xl font-semibold tracking-[-0.02em]">Находки</h1>
        <p className="text-ink-2">Дефекты требований по критериям ISO/IEC/IEEE 29148 и IEEE 830.</p>
      </header>

      <section aria-label="Фильтры" className="space-y-3 rounded-2xl bg-surface p-4 ring-1 ring-line">
        {requirement && (
          <p className="flex flex-wrap items-center gap-2 rounded-lg bg-accent-soft px-3 py-2 text-sm">
            <span>
              Требование <b>{requirement.id}</b>: {requirement.text}
            </span>
            <button
              type="button"
              onClick={() => selectRequirement(null)}
              className="cursor-pointer font-semibold text-accent-ink hover:underline"
            >
              Показать все
            </button>
          </p>
        )}
        <Group title="Важность">
          {[...SEVERITY_ORDER].reverse().map((s) => (
            <Chip
              key={s}
              active={filters.severities.includes(s)}
              onClick={() => toggleFilter('severities', s)}
              count={count((f) => f.severity, s)}
            >
              {SEVERITY[s].label}
            </Chip>
          ))}
        </Group>
        <Group title="Область">
          {SCOPES.map((s) => (
            <Chip key={s.value} active={filters.scope === s.value} onClick={() => setFilters({ scope: s.value })}>
              {s.label}
            </Chip>
          ))}
        </Group>
        <Group title="Стадия проверки">
          {STAGES.map((s) => (
            <Chip
              key={s.id}
              active={filters.stages.includes(s.id)}
              onClick={() => toggleFilter('stages', s.id)}
              count={count(findingStage, s.id)}
            >
              {s.label}
            </Chip>
          ))}
        </Group>
        <Group title="Критерий">
          {CRITERIA_KEYS.map((k) => (
            <Chip
              key={k}
              active={filters.criteria.includes(k)}
              onClick={() => toggleFilter('criteria', k)}
              count={count((f) => f.criterion, k)}
            >
              {CRITERIA[k].label}
            </Chip>
          ))}
        </Group>
        <div className="flex items-center justify-between gap-3 border-t border-line pt-3">
          <p role="status" className="tabular text-sm text-ink-2">
            Показано {visible.length} из {report.findings.length}
          </p>
          <Button
            variant="secondary"
            className="!px-3 !py-1.5"
            disabled={!anyFilter}
            onClick={() => {
              resetFilters();
              selectRequirement(null);
            }}
          >
            Сбросить фильтры
          </Button>
        </div>
      </section>

      {visible.length === 0 ? (
        <div className="space-y-2 rounded-2xl bg-surface p-6 ring-1 ring-line">
          <p className="font-semibold">По этим фильтрам находок нет</p>
          <p className="text-ink-2">Ослабьте условия или сбросьте фильтры.</p>
        </div>
      ) : (
        <ul className="space-y-4">
          {visible.map((f) => (
            <li key={f.id}>
              <FindingCard finding={f} docLink selected={f.id === selectedFindingId} onSelect={() => selectFinding(f.id)} />
            </li>
          ))}
        </ul>
      )}
      <p className="text-xs text-muted">
        Откройте <Link className="text-accent-ink hover:underline" to="/document">документ с подсветкой</Link>, чтобы увидеть находки в тексте.
      </p>
    </div>
  );
}

export function FindingsScreen() {
  return <ReportGate>{(report) => <FindingsView report={report} />}</ReportGate>;
}
