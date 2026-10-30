// Generic data table. columns: [{ key, label, num?, render?(row) }]

import { h } from '../util/dom.js';
import { fmtCell } from '../util/format.js';

export function dataTable({ columns, rows, onRow, empty = 'No rows' }) {
  if (!rows.length) return h('div', { class: 'table-wrap' }, h('div', { class: 'empty' }, empty));
  const head = h('tr', null, columns.map((c) => h('th', null, c.label ?? c.key)));
  const body = rows.map((row) =>
    h(
      'tr',
      { class: onRow ? 'clickable' : null, onclick: onRow ? () => onRow(row) : null },
      columns.map((c) => {
        const raw = row[c.key];
        const cls = c.num ? 'num' : typeof raw === 'object' && raw !== null ? 'cell-json' : typeof raw === 'string' && raw.length > 60 ? 'cell-long' : null;
        const text = fmtCell(raw);
        return h('td', { class: cls, title: cls && cls !== 'num' ? text : null }, c.render ? c.render(row) : text);
      }),
    ),
  );
  return h('div', { class: 'table-wrap' }, h('table', null, h('thead', null, head), h('tbody', null, body)));
}

export const stateBox = (msg, kind = '') => h('div', { class: `state ${kind}` }, msg);
