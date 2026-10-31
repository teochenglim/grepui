// Metrics: PromQL range and instant queries (TQL EVAL), a metric list with labels, SQL on metric
// tables. The URL carries chart, kind, stream, q and range, so a link reproduces the page.

import * as api from '../api.js';
import { barList, seriesName, seriesPlot } from '../components/seriesplot.js';
import { dataTable, stateBox } from '../components/table.js';
import { currentQuery, replaceQuery } from '../router.js';
import { timeWindow } from '../state.js';
import { copyText, h, mount } from '../util/dom.js';
import { fmtNum } from '../util/format.js';

const CHARTS = ['line', 'bar', 'table'];
const KINDS = ['promql', 'sql'];
const pick = (v, set, d) => (set.includes(v) ? v : d);

export async function metricsPage(view) {
  const qp = currentQuery();
  const st = { chart: pick(qp.get('chart'), CHARTS, 'line'), kind: pick(qp.get('kind'), KINDS, 'promql'), stream: qp.get('stream') || '', q: qp.get('q') || '' };
  const sync = () => replaceQuery({ chart: st.chart === 'line' ? null : st.chart, kind: st.kind === 'promql' ? null : st.kind, stream: st.stream, q: st.q });

  const list = h('div', { class: 'metric-list' }, stateBox('Loading…'));
  const labels = h('div', { class: 'row' });
  const out = h('div', null, h('div', { class: 'empty' }, 'Pick a metric or write a query, then Run.'));
  const box = h('textarea', { rows: 3, spellcheck: 'false', class: 'sqlbox', 'aria-label': 'Query' });
  box.value = st.q;
  const kindSel = h('select', { 'aria-label': 'Query language' }, KINDS.map((k) => h('option', { value: k, selected: k === st.kind }, k === 'promql' ? 'PromQL' : 'SQL')));
  const chartSel = h('select', { 'aria-label': 'Chart type' }, CHARTS.map((c) => h('option', { value: c, selected: c === st.chart }, c)));
  const note = h('span', { class: 'muted' });

  async function run() {
    st.q = box.value;
    st.kind = kindSel.value;
    st.chart = chartSel.value;
    sync();
    mount(out, stateBox('Running…'));
    const w = timeWindow();
    try {
      if (st.kind === 'sql') {
        const { data, meta } = await api.get('sql', { sql: st.q, from: w.from, to: w.to, limit: 500 });
        mount(out, h('p', { class: 'muted' }, `${fmtNum(data.rows.length)} rows in ${fmtNum(meta.ms)} ms${data.truncated ? ' (truncated)' : ''}`), dataTable({ rows: data.rows, columns: data.columns.map((key) => ({ key })), empty: 'The query returned no rows.' }));
        return;
      }
      const { data, meta } = await api.get('metrics/query', { expr: st.q, from: w.from, to: w.to });
      note.textContent = `step ${data.step_s}s · ${fmtNum(data.series.length)} series · ${fmtNum(meta.ms)} ms`;
      if (!data.series.length) return mount(out, h('div', { class: 'empty' }, 'No data. Check the metric name and the time range.'));
      const body = st.chart === 'table'
        ? dataTable({ rows: data.series.map((s) => ({ series: seriesName(s), last: s.points.length ? s.points[s.points.length - 1][1] : null, points: s.points.length })), columns: [{ key: 'series' }, { key: 'last', num: true }, { key: 'points', num: true }] })
        : st.chart === 'bar' ? barList(data.series) : seriesPlot(data.series, data.start_s * 1000, data.end_s * 1000);
      mount(out, data.truncated ? h('div', { class: 'callout warn' }, 'The result was cut at the sample limit. Narrow the range or aggregate.') : null, body);
    } catch (e) {
      mount(out, h('div', { class: 'callout warn' }, e.message));
    }
  }

  async function pickMetric(name, fill = true) {
    st.stream = name;
    if (fill && st.kind === 'promql') box.value = name;
    sync();
    try {
      const { data } = await api.get(`metrics/${encodeURIComponent(name)}/labels`);
      mount(labels, h('span', { class: 'muted' }, 'labels:'), data.map((l) => h('span', { class: 'chip-static' }, l)));
    } catch {
      mount(labels);
    }
  }

  try {
    const { data } = await api.get('metrics');
    mount(list, data.length ? data.map((m) => h('button', { type: 'button', class: `metric${m.name === st.stream ? ' sel' : ''}`, onclick: () => (pickMetric(m.name), [...list.children].forEach((c) => c.classList.toggle('sel', c.dataset.n === m.name))), 'data-n': m.name }, h('span', { class: 'clip' }, m.name), h('span', { class: 'badge' }, m.kind))) : h('div', { class: 'empty' }, 'No metrics yet. See Connect to send OTLP or Prometheus data.'));
  } catch (e) {
    mount(list, stateBox(e.message, 'error'));
  }
  if (st.stream) pickMetric(st.stream, false);

  const go = h('button', { class: 'primary', type: 'button', onclick: run }, 'Run (Ctrl+Enter)');
  box.addEventListener('keydown', (e) => e.key === 'Enter' && (e.ctrlKey || e.metaKey) && (e.preventDefault(), run()));
  const share = h('button', { type: 'button', onclick: async () => { st.q = box.value; st.kind = kindSel.value; st.chart = chartSel.value; sync(); await copyText(location.href); share.textContent = 'Link copied'; setTimeout(() => (share.textContent = 'Copy link'), 1500); } }, 'Copy link');
  mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Metrics'), h('span', { class: 'sub' }, 'PromQL over your metric tables, or SQL')),
    h('div', { class: 'split' }, h('div', { class: 'card fieldbox' }, h('h2', null, 'Metrics'), list),
      h('div', { class: 'grow stack' },
        h('div', { class: 'card stack' }, box, h('div', { class: 'row' }, kindSel, chartSel, go, share, note), labels,
          h('p', { class: 'muted' }, st.kind === 'sql' ? 'SQL mode rules apply (one read-only statement in this database).' : 'Rate, sum by, histogram_quantile and the rest of PromQL are supported by GreptimeDB.')),
        h('div', { class: 'card' }, out))));
  if (st.q) run();
}
