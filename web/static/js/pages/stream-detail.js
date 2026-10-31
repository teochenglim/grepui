// One stream: schema with roles, exact stats, and the newest rows in the window.

import * as api from '../api.js';
import { stat } from '../components/stat.js';
import { dataTable, stateBox } from '../components/table.js';
import { href } from '../router.js';
import { timeWindow } from '../state.js';
import { h, mount } from '../util/dom.js';
import { fmtAge, fmtBytes, fmtNum, fmtTime } from '../util/format.js';

export async function streamDetailPage(view, { name }) {
  view.append(stateBox('Loading…'));
  const { data: schema } = await api.get(`streams/${encodeURIComponent(name)}`);
  const statsBox = h('div', { class: 'grid' }, stateBox('Counting rows…'));
  const sampleBox = h('div', null, stateBox('Loading rows…'));
  const detailsBox = h('div', { class: 'card' }, h('h2', null, 'Storage and indexes'), stateBox('Loading…'));

  mount(
    view,
    h('div', { class: 'page-head' }, h('a', { href: href('streams') }, '← Streams'), h('h2', null, name), h('span', { class: 'sub' }, schema.time_index ? `time index: ${schema.time_index}` : 'no time index')),
    statsBox,
    detailsBox,
    h('div', { class: 'card' }, h('h2', null, 'Schema'), dataTable({
      rows: schema.columns,
      columns: [
        { key: 'name', label: 'Column', render: (c) => h('code', null, c.name) },
        { key: 'type', label: 'Type' },
        { key: 'role', label: 'Role', render: (c) => h('span', { class: `badge ${c.role}` }, c.role) },
      ],
    })),
    h('div', { class: 'card' }, h('h2', null, 'Newest rows in the selected range'), sampleBox),
  );

  // Table facts and DDL-derived settings (R-S4): size, TTL, indexes, partitions.
  Promise.all([api.get(`streams/${encodeURIComponent(name)}/info`), api.get(`streams/${encodeURIComponent(name)}/ddl`)]).then(([{ data: i }, { data: d }]) => {
    const p = d.parsed;
    const row = (k, v) => h('div', { class: 'kv' }, h('code', null, k), h('span', null, v));
    mount(detailsBox, h('h2', null, 'Storage and indexes'), h('div', { class: 'cols' },
      h('div', null, row('Engine', i.engine), row('Rows (estimate)', fmtNum(i.rows_estimate)), row('Data size', i.data_bytes === 0 && i.rows_estimate > 0 ? '0 B (in memory; counted after flush)' : fmtBytes(i.data_bytes)), row('Index size', fmtBytes(i.index_bytes)), row('Kind', i.kind)),
      h('div', null,
        row('TTL (retention)', p.ttl || 'none: kept forever'),
        row('Append mode', p.append_mode ? 'yes (rows cannot be deleted)' : 'no'),
        row('Time index', p.time_index || '–'),
        row('Primary key', p.primary_key.length ? p.primary_key.join(', ') : '–'),
        row('Partitioned on', p.partition_columns.length ? p.partition_columns.join(', ') : 'not partitioned'),
        row('Indexes', p.indexes.length ? p.indexes.map(([c, k]) => `${c} (${k})`).join(', ') : 'none')),
    ), h('details', null, h('summary', null, 'CREATE TABLE'), h('pre', null, d.ddl)));
  }).catch((e) => mount(detailsBox, h('h2', null, 'Storage and indexes'), stateBox(e.message, 'error')));

  // Stats and sample are independent calls so a slow count never blocks the rows.
  api.get(`streams/${encodeURIComponent(name)}/stats`).then(({ data: s }) =>
    mount(statsBox, stat('Rows', fmtNum(s.rows)), stat('Oldest', fmtTime(s.oldest)), stat('Newest', fmtTime(s.newest), fmtAge(s.newest))),
  ).catch((e) => mount(statsBox, stateBox(e.message, 'error')));

  const w = timeWindow();
  api.get(`streams/${encodeURIComponent(name)}/sample`, { from: w.from, to: w.to, limit: 50 }).then(({ data: rows }) =>
    mount(sampleBox, dataTable({ rows, columns: Object.keys(rows[0] || {}).map((key) => ({ key, render: key === schema.time_index ? (r) => fmtTime(r[key]) : undefined })), empty: 'No rows in this range. Widen the time range above.' })),
  ).catch((e) => mount(sampleBox, stateBox(e.message, 'error')));
}
