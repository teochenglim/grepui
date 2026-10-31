// RUM (R-U1): page views, INP, JavaScript errors and sessions from the tables GrepRUM writes.

import * as api from '../api.js';
import { dataTable, stateBox } from '../components/table.js';
import { histogram } from '../components/histogram.js';
import { timeWindow } from '../state.js';
import { h, mount } from '../util/dom.js';
import { fmtNum, fmtTime } from '../util/format.js';

const TABS = [['overview', 'Overview'], ['vitals', 'Web vitals'], ['errors', 'Errors'], ['sessions', 'Sessions']];
const stat = (label, value, note) => h('div', { class: 'card stat' }, h('div', { class: 'label' }, label), h('div', { class: 'value' }, value), note ? h('div', { class: 'hint' }, note) : null);
const ms = (v) => (v == null ? '–' : `${Math.round(Number(v))} ms`);
const rating = (v, good, poor) => (v == null ? '' : Number(v) <= good ? 'ok' : Number(v) <= poor ? 'warn' : 'bad');

export async function rumPage(view, _p, tab = 'overview') {
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  const siteSel = h('select', { 'aria-label': 'Site', hidden: true });
  mount(view,
    h('div', { class: 'page-head' }, h('h2', null, 'RUM'), h('span', { class: 'sub' }, 'browser data from GrepRUM'), h('span', { class: 'grow' }), siteSel),
    h('div', { class: 'tabs', role: 'tablist' }, TABS.map(([k, l]) => h('button', { type: 'button', role: 'tab', class: 'tab', 'aria-selected': k === tab, onclick: () => rumPage(view, _p, k) }, l))), out);

  const params = () => ({ ...timeWindow(), site: siteSel.value || null });
  try {
    const st = (await api.get('rum/status')).data;
    if (!st.installed) {
      mount(out, h('div', { class: 'card stack' }, h('h3', null, 'GrepRUM is not installed for this org'),
        h('p', null, `This org's database has no GrepRUM "${st.table}" table${st.missing.length && st.missing.length < 7 ? `, or it lacks the columns ${st.missing.join(', ')}` : ''}. Install the GrepRUM tracker and gateway and point its sink at this org's database, then reload this page.`),
        h('p', { class: 'muted' }, 'grepui only reads these tables; it never creates them.')));
      return;
    }
    const sites = (await api.get('rum/sites', timeWindow())).data;
    if (sites.length > 1) {
      mount(siteSel, [h('option', { value: '' }, 'All sites'), ...sites.map((s) => h('option', { value: s }, s))]);
      siteSel.hidden = false;
      siteSel.onchange = () => load();
    }
  } catch (e) {
    mount(out, stateBox(e.message, 'error'));
    return;
  }

  const views = {
    async overview() {
      const p = params();
      const { data: d } = await api.get('rum/overview', p);
      const t = d.totals || {};
      mount(out,
        h('div', { class: 'row' }, stat('Page views', fmtNum(t.pageviews)), stat('Sessions', fmtNum(t.sessions)), stat('Visitors', fmtNum(t.visitors)), stat('JS errors', fmtNum(t.errors))),
        h('div', { class: 'card' }, histogram({ buckets: d.series.map((b) => ({ t: b.bucket_ms, count: b.pageviews })), bucket_ms: d.bucket_ms, total: t.pageviews || 0 }, p.from, p.to, 'page views')),
        h('div', { class: 'card stack' }, h('h3', null, 'Top pages'), dataTable({ columns: [{ key: 'path', label: 'Page' }, { key: 'views', label: 'Views', num: true, render: (r) => fmtNum(r.views) }, { key: 'sessions', label: 'Sessions', num: true, render: (r) => fmtNum(r.sessions) }], rows: d.pages, empty: 'No page views in this range.' })));
    },
    async vitals() {
      const { data: d } = await api.get('rum/vitals', params());
      const o = d.overall || {};
      const badge = (v) => h('span', { class: `badge ${rating(v, d.good_ms, d.poor_ms)}` }, ms(v));
      const share = (n) => (o.samples ? `${Math.round((100 * Number(n || 0)) / o.samples)}%` : '–');
      mount(out,
        h('p', { class: 'muted' }, `Interaction to Next Paint (INP), the only web vital GrepRUM collects. Good is up to ${d.good_ms} ms, poor is over ${d.poor_ms} ms.`),
        h('div', { class: 'row' }, stat('Samples', fmtNum(o.samples)), stat('p50', ms(o.p50)), stat('p75', ms(o.p75), 'the figure Google rates'), stat('p95', ms(o.p95)),
          stat('Good', share(o.good)), stat('Needs improvement', share(o.needs_improvement)), stat('Poor', share(o.poor))),
        h('div', { class: 'card stack' }, h('h3', null, 'By page, slowest first'), dataTable({ columns: [{ key: 'path', label: 'Page' }, { key: 'samples', label: 'Samples', num: true }, { key: 'p50', label: 'p50', render: (r) => badge(r.p50) }, { key: 'p75', label: 'p75', render: (r) => badge(r.p75) }, { key: 'p95', label: 'p95', render: (r) => badge(r.p95) }], rows: d.pages, empty: 'No INP samples in this range.' })));
    },
    async errors() {
      const { data } = await api.get('rum/errors', { ...params(), limit: 100 });
      mount(out, dataTable({
        columns: [{ key: 'message', label: 'Message' }, { key: 'occurrences', label: 'Times', num: true }, { key: 'sessions', label: 'Sessions', num: true }, { key: 'path', label: 'Page' },
          { key: 'last_ms', label: 'Last seen', render: (r) => fmtTime(r.last_ms) },
          { key: 'stack', label: 'Stack', render: (r) => (r.stack ? h('details', null, h('summary', null, 'stack'), h('pre', { class: 'sqlbox' }, r.stack)) : '–') }],
        rows: data, empty: 'No JavaScript errors in this range.',
      }));
    },
    async sessions() {
      const { data } = await api.get('rum/sessions', { ...params(), limit: 100 });
      mount(out, dataTable({
        columns: [{ key: 'session_id', label: 'Session' }, { key: 'started_ms', label: 'Started', render: (r) => fmtTime(r.started_ms) },
          { key: 'events', label: 'Events', num: true }, { key: 'pageviews', label: 'Pages', num: true }, { key: 'errors', label: 'Errors', num: true }, { key: 'last_path', label: 'Last page' }],
        rows: data, empty: 'No sessions in this range.',
      }));
    },
  };
  async function load() {
    try { mount(out, stateBox('Loading…')); await views[tab](); } catch (e) { mount(out, stateBox(e.message, 'error')); }
  }
  load();
}
