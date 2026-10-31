// SQL mode: one read-only statement, window macros, results as a table.

import * as api from '../api.js';
import { dataTable, stateBox } from './table.js';
import { timeWindow } from '../state.js';
import { h, mount } from '../util/dom.js';
import { fmtNum } from '../util/format.js';

const EXAMPLE = `SELECT severity_text, count(*) AS n
FROM opentelemetry_logs
WHERE timestamp >= $__from AND timestamp < $__to
GROUP BY 1
ORDER BY n DESC`;

export function sqlMode({ sql, onChange }) {
  const area = h('textarea', { rows: 8, spellcheck: 'false', 'aria-label': 'SQL', class: 'sqlbox' });
  area.value = sql || EXAMPLE;
  const out = h('div');
  const run = h('button', { class: 'primary', type: 'button' }, 'Run (Ctrl+Enter)');
  async function exec() {
    onChange(area.value);
    mount(out, stateBox('Running…'));
    try {
      const w = timeWindow();
      const { data, meta } = await api.get('sql', { sql: area.value, from: w.from, to: w.to, limit: 500 });
      mount(out,
        h('p', { class: 'muted' }, `${fmtNum(data.rows.length)} rows in ${fmtNum(meta.ms)} ms${data.truncated ? ' (truncated at the row limit)' : ''}`),
        dataTable({ rows: data.rows, columns: data.columns.map((key) => ({ key })), empty: 'The query returned no rows.' }));
    } catch (e) {
      mount(out, h('div', { class: 'callout warn' }, e.message));
    }
  }
  run.addEventListener('click', exec);
  area.addEventListener('keydown', (e) => e.key === 'Enter' && (e.ctrlKey || e.metaKey) && (e.preventDefault(), exec()));
  return h('div', { class: 'sqlmode' },
    h('p', { class: 'muted' }, 'One SELECT, WITH, SHOW, DESC or EXPLAIN statement, inside this org\'s database (PromQL arrives with Metrics). ', h('code', null, '$__from'), ' and ', h('code', null, '$__to'), ' become the selected time range.'),
    area, h('div', { class: 'row' }, run), out);
}
