// Connect: how to send telemetry to GreptimeDB, built from the real configured address.

import * as api from '../api.js';
import { copybox } from '../components/copybox.js';
import { stateBox } from '../components/table.js';
import { h, mount } from '../util/dom.js';
import { fmtNum } from '../util/format.js';

const SNIPPETS = [
  ['otel_collector', 'OTel Collector'],
  ['sdk_env', 'SDK environment'],
  ['prometheus', 'Prometheus'],
  ['check_tables', 'Check tables'],
];

export async function connectPage(view) {
  view.append(stateBox('Loading…'));
  const [{ data: c }, streams] = await Promise.all([api.get('connect'), api.get('streams').catch(() => null)]);

  // "Waiting for data" check: green once traces/logs tables have rows.
  const rows = (kind) => streams?.data.filter((s) => s.kind === kind).reduce((n, s) => n + (Number(s.rows_estimate) || 0), 0) ?? null;
  const chip = (label, n) => h('span', { class: `badge ${n > 0 ? 'ok' : n === 0 ? 'warn' : ''}` }, n == null ? `${label}: unknown` : n > 0 ? `${label}: ${fmtNum(n)} rows` : `${label}: waiting for data`);

  const out = h('div');
  const tabs = h('div', { class: 'tabs', role: 'tablist' });
  const paint = (key) => {
    mount(tabs, SNIPPETS.map(([k, label]) => h('button', { class: 'tab', role: 'tab', 'aria-selected': String(k === key), onclick: () => paint(k) }, label)));
    mount(out, copybox(c.snippets[key]));
  };
  paint(SNIPPETS[0][0]);

  mount(
    view,
    h('div', { class: 'page-head' }, h('h2', null, 'Connect'), h('span', { class: 'sub' }, `org ${c.org} → database ${c.database} at ${c.http_base}`)),
    h('div', { class: 'row' }, chip('Traces', rows('traces')), chip('Logs', rows('logs'))),
    h('div', { class: 'callout warn' }, h('strong', null, 'Read this first'), h('ul', null, c.notes.map((n) => h('li', null, n)))),
    h('div', { class: 'card' }, h('h2', null, 'Endpoints'), h('div', { class: 'table-wrap' }, h('table', null,
      h('thead', null, h('tr', null, ['Signal', 'Request', 'Headers'].map((t) => h('th', null, t)))),
      h('tbody', null, c.endpoints.map((e) => h('tr', null,
        h('td', null, h('span', { class: 'badge' }, e.signal)),
        h('td', null, h('code', null, `${e.method} ${e.url}`), h('div', { class: 'muted' }, e.content_type)),
        h('td', null, Object.entries(e.headers).map(([k, v]) => h('div', null, h('code', null, `${k}: ${v}`)))),
      )))))),
    h('div', { class: 'card' }, h('h2', null, 'Snippets'), tabs, out),
  );
}
