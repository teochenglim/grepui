// The expanded record: every field, the raw JSON, and Search Around.

import * as api from '../api.js';
import { copyText, h, mount } from '../util/dom.js';
import { fmtTime } from '../util/format.js';
import { levelClass } from '../util/logs.js';
import { href } from '../router.js';

const kv = (obj) => Object.entries(obj || {}).map(([k, v]) => h('div', { class: 'kv' }, h('code', null, k), h('span', null, typeof v === 'object' ? JSON.stringify(v) : String(v))));

export function recordDetail(r) {
  const around = h('div', { class: 'around' });
  const aroundBtn = h('button', { type: 'button' }, 'Search around');
  aroundBtn.addEventListener('click', async () => {
    mount(around, h('span', { class: 'muted' }, 'Loading neighbours…'));
    try {
      const { data } = await api.get('logs/around', { ts_ns: r.ts_ns, n: 20 });
      mount(around, h('h3', null, 'Around this line (storage order, filters ignored)'), h('div', { class: 'table-wrap' }, h('table', { class: 'logs compact' }, h('tbody', null, data.rows.map((x) => h('tr', { class: x.ts_ns === r.ts_ns ? 'pivot' : null }, h('td', { class: 'mono nowrap' }, fmtTime(x.timestamp)), h('td', null, h('span', { class: `badge ${levelClass(x.severity_text)}` }, x.severity_text || '–')), h('td', { class: 'msg' }, x.body ?? '')))))));
    } catch (e) {
      mount(around, h('span', { class: 'err' }, e.message));
    }
  });
  const raw = JSON.stringify(r, null, 2);
  const copy = h('button', { class: 'ghost', type: 'button' }, 'Copy JSON');
  copy.addEventListener('click', async () => {
    copy.textContent = (await copyText(raw)) ? 'Copied' : 'Select and copy';
    setTimeout(() => (copy.textContent = 'Copy JSON'), 1500);
  });
  const trace = r.trace_id ? h('span', { class: 'kv' }, h('code', null, 'trace_id'), h('span', { class: 'mono', title: 'Trace view arrives in v0.4' }, r.trace_id)) : null;
  return h(
    'div',
    { class: 'record' },
    h('div', { class: 'row' }, aroundBtn, copy, h('span', { class: 'muted' }, `ts_ns ${r.ts_ns}`)),
    h('div', { class: 'cols' },
      h('div', null, h('h3', null, 'Log attributes'), kv(r.log_attributes), h('h3', null, 'Resource'), kv(r.resource_attributes)),
      h('div', null, h('h3', null, 'Context'), kv({ scope: r.scope_name, span_id: r.span_id, severity_number: r.severity_number }), trace)),
    h('details', null, h('summary', null, 'Raw record'), h('pre', null, raw)),
    around,
  );
}
export { href };
