// Logs: filter bar, histogram, fields, results with paging, SQL mode, export.

import * as api from '../api.js';
import { fieldList } from '../components/fieldlist.js';
import { filterBar } from '../components/filterbar.js';
import { histogram } from '../components/histogram.js';
import { logTable } from '../components/logtable.js';
import { sqlMode } from '../components/sqlmode.js';
import { stateBox } from '../components/table.js';
import { currentQuery, replaceQuery } from '../router.js';
import { timeWindow } from '../state.js';
import { addCondition, cond, filterKey } from '../util/logs.js';
import { h, mount } from '../util/dom.js';
import { fmtNum } from '../util/format.js';

const PAGE = 100;

export async function logsPage(view) {
  const qp = currentQuery();
  const st = { q: qp.get('q') || '', level: qp.get('level') || '', mode: qp.get('mode') === 'sql' ? 'sql' : 'search', sql: qp.get('sql') || '' };
  const w = () => ({ ...timeWindow() });
  const params = () => {
    const { from, to } = w();
    return { from, to, q: st.q || null, level: st.level || null };
  };

  const bar = filterBar({ q: st.q, level: st.level, onSubmit: (v) => { Object.assign(st, v); sync(); load(); } });
  const histBox = h('div', { class: 'card' });
  const results = h('div');
  const fieldBox = h('div');
  const more = h('button', { type: 'button' }, 'Load more');
  const summary = h('span', { class: 'muted' });
  let rows = [];
  let offset = 0;
  let epoch = 0;

  const sync = () => replaceQuery({ q: st.q, level: st.level, mode: st.mode === 'sql' ? 'sql' : null, sql: st.mode === 'sql' ? st.sql : null });

  function paintRows(note) {
    mount(results, logTable(rows), rows.length && rows.length % PAGE === 0 ? h('div', { class: 'row center' }, more) : null);
    mount(summary, `${fmtNum(rows.length)} shown`, note ? h('span', { class: 'badge warn', title: note }, 'LIKE fallback') : null);
  }

  async function load() {
    const mine = ++epoch;
    offset = 0;
    mount(results, stateBox('Searching…'));
    mount(histBox, stateBox('Counting…'));
    const p = params();
    const [search, hist] = await Promise.allSettled([api.get('logs', { ...p, limit: PAGE, offset: 0 }), api.get('logs/histogram', p)]);
    if (mine !== epoch) return; // a newer search started
    if (search.status === 'fulfilled') {
      rows = search.value.data;
      paintRows(search.value.meta.note);
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
      mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Logs'), h('div', { class: 'seg' }, modeBtn('search', 'Search'), modeBtn('sql', 'SQL'))),
        sqlMode({ sql: st.sql, onChange: (s) => { st.sql = s; sync(); } }));
      return;
    }
    mount(view,
      h('div', { class: 'page-head' }, h('h2', null, 'Logs'), h('div', { class: 'seg' }, modeBtn('search', 'Search'), modeBtn('sql', 'SQL')), h('span', { class: 'grow' }), summary, h('span', { class: 'muted' }, 'Export'), exportLink('csv'), exportLink('ndjson')),
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
