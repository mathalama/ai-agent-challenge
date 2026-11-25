import { SEVERITY } from '../lib/labels';
import type { Severity } from '../lib/types';

interface Props {
  severity: Severity;
  className?: string;
}

export function SeverityBadge({ severity, className = '' }: Props) {
  const s = SEVERITY[severity];
  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-semibold whitespace-nowrap ${s.bg} ${s.text} ${className}`}
    >
      <span aria-hidden className={`size-1.5 rounded-full ${s.dot}`} />
      {s.label}
    </span>
  );
}
