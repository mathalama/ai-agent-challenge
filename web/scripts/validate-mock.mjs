// Проверяет web/public/mock/report.json по docs/api/report.schema.json
// и сверяет цитаты находок и вопросов с текстом блоков документа.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import Ajv2020 from 'ajv/dist/2020.js';
import addFormats from 'ajv-formats';

const here = dirname(fileURLToPath(import.meta.url));
const read = (p) => JSON.parse(readFileSync(resolve(here, p), 'utf8'));

const schema = read('../../docs/api/report.schema.json');
const report = read('../public/mock/report.json');
const trace = read('../public/mock/stages.json');

const ajv = new Ajv2020({ allErrors: true, strict: false });
addFormats(ajv);
// Форматы Rust-типов: диапазон уже задан через minimum/maximum.
for (const format of ['uint', 'uint8']) ajv.addFormat(format, true);
const validate = ajv.compile(schema);

const errors = [];
if (!validate(report)) {
  for (const e of validate.errors) errors.push(`схема: ${e.instancePath || '/'} ${e.message}`);
}

// --- Цитаты дословно присутствуют в блоках, перекрывающих указанные строки ---
const norm = (s) => s.replace(/\s+/g, ' ').trim();
function blockText(block, start, end) {
  if (block.kind !== 'table') return block.text;
  const lines = block.row_lines ?? [];
  return block.rows
    .filter((_, i) => lines[i] >= start && lines[i] <= end)
    .map((row) => row.join(' '))
    .join('\n');
}
function quoteFound({ quote, location }) {
  const { start_line: start, end_line: end, page } = location;
  const text = report.blocks
    .filter((b) => b.location.page === page && b.location.end_line >= start && b.location.start_line <= end)
    .map((b) => blockText(b, start, end))
    .join('\n');
  return norm(text).includes(norm(quote));
}
const checkEvidence = (owner, list) =>
  list.forEach((ev, i) => {
    if (!quoteFound(ev)) errors.push(`цитата: ${owner}, доказательство ${i + 1} не найдено в тексте: «${ev.quote.slice(0, 60)}»`);
  });
report.findings.forEach((f) => checkEvidence(`находка ${f.id}`, f.evidence));
report.questions.forEach((q) => checkEvidence(`вопрос ${q.id}`, q.evidence));

// --- Ссылки и сводка ---
const reqIds = new Set(report.requirements.map((r) => r.id));
const findingIds = new Set(report.findings.map((f) => f.id));
if (findingIds.size !== report.findings.length) errors.push('ссылки: повторяются id находок');
report.findings.forEach((f) => {
  if (f.scope.kind === 'requirement' && !reqIds.has(f.scope.id)) errors.push(`ссылки: ${f.id} → неизвестное требование ${f.scope.id}`);
});
report.questions.forEach((q) =>
  q.related.forEach((r) => {
    const known = r.kind === 'finding' ? findingIds : reqIds;
    if (!known.has(r.id)) errors.push(`ссылки: вопрос ${q.id} → неизвестный ${r.kind} ${r.id}`);
  }),
);
const s = report.summary;
const verified = report.findings.filter((f) => f.verified).length;
const critical = report.findings.filter((f) => f.severity === 'critical').map((f) => f.id);
if (s.requirement_count !== report.requirements.length) errors.push('сводка: requirement_count не совпадает');
if (s.finding_count !== report.findings.length) errors.push('сводка: finding_count не совпадает');
if (s.verified_count !== verified) errors.push('сводка: verified_count не совпадает');
if (JSON.stringify([...s.critical_findings].sort()) !== JSON.stringify(critical.sort())) errors.push('сводка: critical_findings не совпадает');

// --- Трасса стадий ---
const STAGES = ['segment', 'facts', 'rules', 'requirement_checks', 'set_checks', 'consistency', 'ambiguity', 'verify', 'clarify'];
if (JSON.stringify(trace.stages.map((x) => x.stage)) !== JSON.stringify(STAGES)) errors.push('stages.json: порядок или состав стадий неверный');

if (errors.length) {
  console.error(errors.join('\n'));
  process.exit(1);
}
console.log(
  `OK: схема v${report.schema_version}, ${report.requirements.length} требований, ${report.findings.length} находок ` +
    `(подтверждено ${verified}), ${report.questions.length} вопросов, ${trace.stages.length} стадий`,
);
