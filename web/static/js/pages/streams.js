// Streams: every table in the org's database, with what it holds.

import * as api from '../api.js';
import { dataTable, stateBox } from '../components/table.js';
import { href } from '../router.js';
import { h, mount } from '../util/dom.js';
import { fmtNum } from '../util/format.js';

export async function streamsPage(view) {
  view.append(stateBox('Loading streams…'));
  const { data } = await api.get('streams');

  const filter = h('input', { type: 'search', placeholder: 'Filter streams', 'aria-label': 'Filter streams' });
  const holder = h('div');
  const paint = () => {
    const q = filter.value.trim().toLowerCase();
    const rows = data.filter((s) => s.name.toLowerCase().includes(q));
    mount(
      holder,
      dataTable({
        rows,
        empty: data.length ? 'No stream matches the filter.' : 'No tables yet. Send some telemetry: see Connect.',
        columns: [
          { key: 'name', label: 'Stream', render: (s) => h('a', { href: href('streams', s.name) }, s.name) },
          { key: 'kind', label: 'Kind', render: (s) => h('span', { class: `badge ${s.kind.startsWith('logs') ? 'logs' : s.kind.startsWith('traces') ? 'traces' : ''}` }, s.kind) },
          { key: 'engine', label: 'Engine' },
          { key: 'rows_estimate', label: 'Rows (estimate)', num: true, render: (s) => fmtNum(s.rows_estimate) },
        ],
      }),
    );
  };
  filter.addEventListener('input', paint);
  mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Streams'), h('span', { class: 'sub' }, `${data.length} tables in this org's database`)), h('div', { class: 'row' }, filter), holder);
  paint();
}
