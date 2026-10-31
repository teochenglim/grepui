// Results: time, level, message; a row expands into the full record with Search Around.

import { levelClass } from '../util/logs.js';
import { h } from '../util/dom.js';
import { fmtTime } from '../util/format.js';
import { recordDetail } from './recorddetail.js';

export function logRow(r, { onAround } = {}) {
  const detail = h('tr', { class: 'detail', hidden: true }, h('td', { colspan: 3 }));
  let loaded = false;
  const row = h(
    'tr',
    { class: 'clickable', tabindex: '0', 'aria-expanded': 'false' },
    h('td', { class: 'mono nowrap' }, fmtTime(r.timestamp)),
    h('td', null, h('span', { class: `badge ${levelClass(r.severity_text)}` }, r.severity_text || '–')),
    h('td', { class: 'msg' }, r.body ?? ''),
  );
  const toggle = () => {
    detail.hidden = !detail.hidden;
    row.setAttribute('aria-expanded', String(!detail.hidden));
    if (!detail.hidden && !loaded) {
      loaded = true;
      detail.firstChild.append(recordDetail(r, { onAround }));
    }
  };
  row.addEventListener('click', toggle);
  row.addEventListener('keydown', (e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), toggle()));
  return [row, detail];
}

export function logTable(rows, opts) {
  if (!rows.length) return h('div', { class: 'table-wrap' }, h('div', { class: 'empty' }, 'No log lines match. Widen the time range or relax the filter.'));
  return h('div', { class: 'table-wrap' }, h('table', { class: 'logs' }, h('thead', null, h('tr', null, ['Time', 'Level', 'Message'].map((t) => h('th', null, t)))), h('tbody', null, rows.flatMap((r) => logRow(r, opts)))));
}
