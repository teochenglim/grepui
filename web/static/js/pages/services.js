// Services: RED (rate, errors, duration) per service over entry spans.

import * as api from '../api.js';
import { dataTable, stateBox } from '../components/table.js';
import { href } from '../router.js';
import { timeWindow } from '../state.js';
import { h, mount } from '../util/dom.js';
import { fmtNum, fmtUs, serviceColor } from '../util/format.js';

export const serviceHref = (name) => href('services', name);

export const fmtRate = (r) => (Number(r) >= 10 ? Number(r).toFixed(0) : Number(r).toFixed(2));
export const errBadge = (pct) => (Number(pct) > 0 ? h('span', { class: `badge ${Number(pct) >= 5 ? 'bad' : 'warn'}` }, `${Number(pct).toFixed(1)}%`) : h('span', { class: 'muted' }, '0%'));

export async function servicesPage(view) {
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Services'), h('span', { class: 'sub' }, 'rate, errors and duration of server and consumer spans')), out);
  const w = timeWindow();
  try {
    const { data } = await api.get('services', { from: w.from, to: w.to });
    mount(out, dataTable({
      rows: data, empty: 'No spans in this range. Widen the time range or see Connect.',
      onRow: (r) => (location.hash = serviceHref(r.service_name)),
      columns: [
        { key: 'service_name', label: 'Service', render: (r) => h('span', null, h('span', { class: 'dot', style: `background:${serviceColor(r.service_name)}` }), h('a', { href: serviceHref(r.service_name) }, r.service_name)) },
        { key: 'requests', label: 'Requests', num: true, render: (r) => fmtNum(r.requests) },
        { key: 'rate_per_s', label: 'Rate /s', num: true, render: (r) => fmtRate(r.rate_per_s) },
        { key: 'error_pct', label: 'Errors', num: true, render: (r) => errBadge(r.error_pct) },
        { key: 'avg_us', label: 'Avg', num: true, render: (r) => fmtUs(r.avg_us) },
        { key: 'p95_us', label: 'p95', num: true, render: (r) => fmtUs(r.p95_us) },
      ],
    }));
  } catch (e) {
    mount(out, stateBox(e.message, 'error'));
  }
}
