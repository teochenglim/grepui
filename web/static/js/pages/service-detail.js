// One service: summary tiles, requests / error % / p95 over time, operations, catalog note.

import * as api from '../api.js';
import { annotationForm, noteView } from '../components/annotation.js';
import { lineChart } from '../components/linechart.js';
import { stat } from '../components/stat.js';
import { dataTable, stateBox } from '../components/table.js';
import { href } from '../router.js';
import { timeWindow } from '../state.js';
import { h, mount } from '../util/dom.js';
import { fmtNum, fmtUs } from '../util/format.js';
import { errBadge, fmtRate } from './services.js';

export async function serviceDetailPage(view, { name }) {
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  mount(view, h('div', { class: 'page-head' }, h('a', { href: href('services') }, '← Services'), h('h2', null, name)), out);
  const w = timeWindow();
  try {
    const { data } = await api.get(`services/${encodeURIComponent(name)}`, { from: w.from, to: w.to });
    const s = data.summary;
    const step = data.bucket_ms;
    const pts = (f) => data.series.map((r) => ({ t: Number(r.bucket_ms), v: f(r) }));
    const pct = (r) => (Number(r.requests) ? (100 * Number(r.errors)) / Number(r.requests) : 0);
    const common = { from: w.from, to: w.to, step };
    const noteBox = h('div', { class: 'card' });
    const paintNote = (a) => {
      mount(noteBox, h('h2', null, 'Catalog note'), noteView(a) || h('p', { class: 'muted' }, 'No note yet.'),
        annotationForm(name, a, (saved) => paintNote(saved && { ...saved })));
    };
    paintNote(data.annotation);
    const traces = `${href('traces')}${href('traces').includes('?') ? '&' : '?'}service=${encodeURIComponent(name)}`;
    mount(out,
      h('div', { class: 'row' }, h('a', { href: traces }, 'View traces'), h('a', { href: href('service-map') }, 'Service map')),
      h('div', { class: 'grid' }, stat('Requests', fmtNum(s.requests), `${fmtRate(s.rate_per_s)} /s`), stat('Errors', errBadge(s.error_pct)), stat('Avg', fmtUs(s.avg_us)), stat('p95', fmtUs(s.p95_us))),
      h('div', { class: 'grid2' },
        h('div', { class: 'card' }, h('h2', null, 'Requests'), lineChart({ ...common, points: pts((r) => Number(r.requests)), fmt: fmtNum, label: 'Requests over time' })),
        h('div', { class: 'card' }, h('h2', null, 'Error rate'), lineChart({ ...common, points: pts(pct), fmt: (v) => `${v.toFixed(1)}%`, label: 'Error percentage over time', cls: 'bad' })),
        h('div', { class: 'card' }, h('h2', null, 'p95 latency'), lineChart({ ...common, points: pts((r) => Number(r.p95_us) || 0), fmt: fmtUs, label: 'p95 latency over time' })),
        noteBox),
      h('div', { class: 'card' }, h('h2', null, 'Operations'),
        dataTable({
          rows: data.operations, empty: 'No entry spans for this service in the range.',
          columns: [
            { key: 'operation', label: 'Operation' },
            { key: 'requests', label: 'Requests', num: true, render: (r) => fmtNum(r.requests) },
            { key: 'errors', label: 'Errors', num: true, render: (r) => errBadge(Number(r.requests) ? (100 * Number(r.errors)) / Number(r.requests) : 0) },
            { key: 'avg_us', label: 'Avg', num: true, render: (r) => fmtUs(r.avg_us) },
            { key: 'p95_us', label: 'p95', num: true, render: (r) => fmtUs(r.p95_us) },
          ],
        })));
  } catch (e) {
    mount(out, stateBox(e.message, 'error'));
  }
}
