// Logs: filter bar, histogram, fields, results with paging, SQL mode, export.

import * as api from '../api.js';
import { fieldList } from '../components/fieldlist.js';
import { filterBar } from '../components/filterbar.js';
import { histogram } from '../components/histogram.js';
import { logTable } from '../components/logtable.js';
import { applySpec, recordSearch, viewsBar } from '../components/viewsbar.js';
import { sqlMode } from '../components/sqlmode.js';
import { stateBox } from '../components/table.js';
import { currentQuery, replaceQuery } from '../router.js';
import { state, timeWindow } from '../state.js';
import { addCondition, cond, filterKey } from '../util/logs.js';
import { h, mount } from '../util/dom.js';
import { fmtNum } from '../util/format.js';

const PAGE = 100;

export async function logsPage(view) {
  const qp = currentQuery();
  // A shared link (?view=<id>): resolve the saved view, write its search into the URL, re-render.
  if (qp.get('view')) {
    try {
      const { data } = await api.get(`views/${encodeURIComponent(qp.get('view'))}`);
      applySpec(data.spec, null);
    } catch (e) {
      mount(view, stateBox(`Could not open the saved view: ${e.message}`, 'error'));
    }
    return;
  }
  const st = { sources: qp.get('sources') || '', q: qp.get('q') || '', level: qp.get('level') || '', mode: qp.get('mode') === 'sql' ? 'sql' : 'search', sql: qp.get('sql') || '' };
  const w = () => ({ ...timeWindow() });
  const params = () => {
    const { from, to } = w();
    return { from, to, q: st.q || null, level: st.level || null };
  };

  // The sources a search can read from (v0.12.0); the picker shows only when there is a choice.
  let sources = [];
  const searchParams = () => ({ ...params(), sources: st.sources || null });
  const sourceNote = h('span', { class: 'row' });
  const picker = h('select', { 'aria-label': 'Source', onchange: () => { st.sources = picker.value; sync(); load(); } });
  const paintPicker = () => {
    mount(picker, [['', 'Primary source'], ['all', 'All sources'], ...sources.filter((s) => s !== 'primary').map((s) => [s, s])].map(([v, l]) => h('option', { value: v, selected: v === st.sources || null }, l)));
    picker.hidden = sources.length < 2;
  };
  api.get('sources').then(({ data }) => { sources = data.map((s) => s.name); paintPicker(); }).catch(() => {});
  const getSpec = () => ({ q: st.q, level: st.level, range: state.range, mode: st.mode, sql: st.mode === 'sql' ? st.sql : '' });
  const views = viewsBar({ kind: 'logs', getSpec });
  const bar = filterBar({ q: st.q, level: st.level, onSubmit: (v) => { Object.assign(st, v); sync(); load(); recordSearch('logs', getSpec()); } });
  const histBox = h('div', { class: 'card' });
  const results = h('div');
  const fieldBox = h('div');
  const more = h('button', { type: 'button' }, 'Load more');
  const summary = h('span', { class: 'muted' });
  let rows = [];
  let offset = 0;
  let epoch = 0;

  const sync = () => replaceQuery({ q: st.q, level: st.level, sources: st.sources || null, mode: st.mode === 'sql' ? 'sql' : null, sql: st.mode === 'sql' ? st.sql : null });

  function paintRows(note) {
    mount(results, logTable(rows), rows.length && rows.length % PAGE === 0 && !st.sources ? h('div', { class: 'row center' }, more) : null);
    mount(summary, `${fmtNum(rows.length)} shown`, note ? h('span', { class: 'badge warn', title: note }, 'LIKE fallback') : null);
  }

  async function load() {
    const mine = ++epoch;
    offset = 0;
    mount(results, stateBox('Searching…'));
    mount(histBox, stateBox('Counting…'));
    const p = params();
    const [search, hist] = await Promise.allSettled([api.get('logs', { ...searchParams(), limit: PAGE, offset: 0 }), api.get('logs/histogram', p)]);
    if (mine !== epoch) return; // a newer search started
    if (search.status === 'fulfilled') {
      rows = search.value.data;
      paintRows(search.value.meta.note);
      // A source that failed is named; the others still answered.
      mount(sourceNote, (search.value.sources || []).filter((s) => !s.ok).map((s) => h('span', { class: 'badge warn', title: s.error }, `${s.name}: ${s.error}`)));
    } else {
      mount(results, stateBox(search.reason.message, 'error'));
    }
    if (hist.status === 'fulfilled') mount(histBox, histogram(hist.value.data, p.from, p.to));
    else mount(histBox, stateBox(hist.reason.message, 'error'));
  }

  more.addEventListener('click', async () => {
    offset += PAGE;
    more.disabled = true;
    try {
      const { data } = await api.get('logs', { ...params(), limit: PAGE, offset });
      rows = rows.concat(data);
      paintRows();
    } catch (e) {
      mount(results, stateBox(e.message, 'error'));
    } finally {
      more.disabled = false;
    }
  });

  const exportLink = (fmt) => {
    const a = h('a', { class: 'btn', href: '#', download: '' }, fmt.toUpperCase());
    a.addEventListener('click', () => {
      const p = params();
      const qs = new URLSearchParams(Object.entries({ ...p, format: fmt, limit: 10000 }).filter(([, v]) => v != null));
      a.href = `/api/v1/orgs/${encodeURIComponent(location.hash.split('/')[1] || 'default')}/logs/export?${qs}`;
    });
    return a;
  };

  const modeBtn = (m, label) => h('button', { type: 'button', 'aria-pressed': String(st.mode === m), onclick: () => { st.mode = m; sync(); render(); } }, label);

  async function render() {
    if (st.mode === 'sql') {
      mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Logs'), h('div', { class: 'seg' }, modeBtn('search', 'Search'), modeBtn('sql', 'SQL')), h('span', { class: 'grow' }), views),
        sqlMode({ sql: st.sql, onChange: (s) => { st.sql = s; sync(); recordSearch('logs', getSpec()); } }));
      return;
    }
    mount(view,
      h('div', { class: 'page-head' }, h('h2', null, 'Logs'), h('div', { class: 'seg' }, modeBtn('search', 'Search'), modeBtn('sql', 'SQL')), h('span', { class: 'grow' }), picker, sourceNote, views, summary, h('span', { class: 'muted' }, 'Export'), exportLink('csv'), exportLink('ndjson')),
      bar.el, histBox,
      h('div', { class: 'split' }, fieldBox, h('div', { class: 'grow' }, results)));
    load();
    try {
      const { data: fields } = await api.get('logs/fields');
      mount(fieldBox, fieldList({
        fields, params,
        onPick: (field, value) => {
          const key = filterKey(field);
          if (key === 'level') st.level = String(value).toUpperCase();
          else st.q = addCondition(st.q, cond(key, value));
          bar.set({ q: st.q, level: st.level });
          sync();
          load();
        },
      }));
    } catch (e) {
      mount(fieldBox, h('div', { class: 'callout' }, e.message));
    }
  }
  await render();
}
