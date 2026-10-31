// Traces: filters, traces over time, latency scatter, and one row per trace.

import * as api from '../api.js';
import { histogram } from '../components/histogram.js';
import { scatter } from '../components/scatter.js';
import { dataTable, stateBox } from '../components/table.js';
import { traceFilters } from '../components/tracefilters.js';
import { currentQuery, href, navigate, replaceQuery } from '../router.js';
import { timeWindow } from '../state.js';
import { h, mount } from '../util/dom.js';
import { fmtNum, fmtTime, fmtUs, serviceColor } from '../util/format.js';
import { kindBadge } from '../components/llmspan.js';

const PAGE = 50;

/** Link to a trace; `at` tells the trace page where to look. */
export function traceHref(id, startMs) {
  const base = href('traces', id);
  return `${base}${base.includes('?') ? '&' : '?'}at=${startMs}`;
}

export async function tracesPage(view) {
  const qp = currentQuery();
  const st = { q: qp.get('q') || '', service: qp.get('service') || '', minMs: qp.get('min_ms') || '', errors: qp.get('errors') === '1' };
  const params = () => {
    const w = timeWindow();
    return { from: w.from, to: w.to, q: st.q || null, service: st.service || null, min_ms: st.minMs || null, errors: st.errors ? true : null };
  };
  const sync = () => replaceQuery({ q: st.q, service: st.service, min_ms: st.minMs, errors: st.errors ? '1' : null });
  const bar = traceFilters({ ...st, onSubmit: (v) => { Object.assign(st, v); sync(); load(); } });
  const histBox = h('div', { class: 'card' });
  const scatBox = h('div', { class: 'card' });
  const results = h('div');
  const more = h('button', { type: 'button' }, 'Load more');
  let rows = [];
  let offset = 0;
  let epoch = 0;

  const paint = () => {
    const max = Math.max(1, ...rows.map((r) => Number(r.duration_us)));
    mount(results,
      dataTable({
        rows, empty: 'No traces match. Widen the time range or relax the filters.',
        columns: [
          { key: 'start_ms', label: 'Start', render: (r) => h('a', { class: 'mono nowrap', href: traceHref(r.trace_id, r.start_ms) }, fmtTime(r.start_ms)) },
          { key: 'root_operation', label: 'Root', render: (r) => h('span', null, h('span', { class: 'dot', style: `background:${serviceColor(r.root_service)}` }), h('strong', null, r.root_service || '?'), h('span', { class: 'muted' }, ` ${r.root_operation || ''}`)) },
          { key: 'duration_us', label: 'Duration', render: (r) => h('div', { class: 'durcell' }, h('span', { class: 'durbar', style: `width:${Math.max(2, (Number(r.duration_us) / max) * 100)}%;background:${Number(r.errors) ? 'var(--bad)' : 'var(--accent)'}` }), h('span', null, fmtUs(r.duration_us))) },
          { key: 'spans', label: 'Spans', num: true, render: (r) => fmtNum(r.spans) },
          { key: 'services', label: 'Services', num: true, render: (r) => fmtNum(r.services) },
          { key: 'errors', label: 'Errors', num: true, render: (r) => (Number(r.errors) ? h('span', { class: 'badge bad' }, fmtNum(r.errors)) : '') },
        ],
        onRow: (r) => (location.hash = traceHref(r.trace_id, r.start_ms)),
      }),
      rows.length && rows.length % PAGE === 0 ? h('div', { class: 'row center' }, more) : null);
  };

  async function load() {
    const mine = ++epoch;
    offset = 0;
    const p = params();
    mount(results, stateBox('Searching…'));
    mount(histBox, h('h2', null, 'Traces over time'), stateBox('Counting…'));
    mount(scatBox, h('h2', null, 'Latency'), stateBox('Plotting…'));
    const [list, hist, sc] = await Promise.allSettled([api.get('traces', { ...p, limit: PAGE, offset: 0 }), api.get('traces/histogram', p), api.get('traces/scatter', p)]);
    if (mine !== epoch) return;
    if (list.status === 'fulfilled') (rows = list.value.data, paint());
    else mount(results, stateBox(list.reason.message, 'error'));
    mount(histBox, h('h2', null, 'Traces over time'), hist.status === 'fulfilled' ? histogram(hist.value.data, p.from, p.to, 'traces') : stateBox(hist.reason.message, 'error'));
    mount(scatBox, h('h2', null, 'Latency'),
      sc.status === 'fulfilled' ? (sc.value.data.points.length ? scatter(sc.value.data, p.from, p.to, (pt) => (location.hash = traceHref(pt.trace_id, pt.start_ms))) : h('div', { class: 'empty' }, 'No traces in this range.')) : stateBox(sc.reason.message, 'error'));
  }

  more.addEventListener('click', async () => {
    offset += PAGE;
    more.disabled = true;
    try {
      rows = rows.concat((await api.get('traces', { ...params(), limit: PAGE, offset })).data);
      paint();
    } catch (e) {
      mount(results, stateBox(e.message, 'error'));
    } finally {
      more.disabled = false;
    }
  });

  // Which attribute layout the table has: JSON tables (trace v2, LLM data) accept any attribute name in the filter.
  const kinds = h('div', { class: 'row wrap', hidden: true });
  const KINDS = ['LLM', 'AGENT', 'CHAIN', 'TOOL', 'RETRIEVER', 'RERANKER', 'EMBEDDING', 'GUARDRAIL', 'EVALUATOR'];
  const setKind = (k) => { const rest = st.q.split(/\s+/).filter((t) => t && !t.startsWith('openinference.span.kind=')); st.q = [...rest, ...(k ? [`openinference.span.kind=${k}`] : [])].join(' '); bar.setQuery?.(st.q); sync(); load(); };
  mount(kinds, h('span', { class: 'muted' }, 'Span kind'), KINDS.map((k) => h('button', { type: 'button', class: 'ghost kind-chip', onclick: () => setKind(k) }, kindBadge(k))), h('button', { type: 'button', class: 'ghost', onclick: () => setKind(null) }, 'any'));
  const layoutBadge = h('span', { class: 'badge', hidden: true });
  api.get('traces/layout').then(({ data }) => { if (data.layout === 'json') { mount(layoutBadge, 'JSON attributes (trace v2)'); layoutBadge.title = 'Filter on any attribute, e.g. llm.model_name=gpt-4o or openinference.span.kind=TOOL'; layoutBadge.hidden = false; kinds.hidden = false; } }).catch(() => {});
  mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Traces'), h('span', { class: 'sub' }, 'one row per trace'), layoutBadge), bar.el, kinds, h('div', { class: 'grid2' }, histBox, scatBox), results);
  load();
  void navigate;
}
