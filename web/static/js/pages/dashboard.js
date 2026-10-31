// One dashboard: view, edit, share, export. Editing changes the spec in the page and saves it
// as one document; the server validates it and returns the canonical form.

import * as api from '../api.js';
import { dashboardView } from '../components/dashview.js';
import { stateBox } from '../components/table.js';
import { currentQuery, href, refresh, replaceQuery } from '../router.js';
import { RANGES, state } from '../state.js';
import { copyText, h, mount } from '../util/dom.js';
import { fmtTime } from '../util/format.js';

const TYPES = ['line', 'area', 'stacked_area', 'bar_v', 'bar_h', 'stacked_bar_v', 'stacked_bar_h', 'scatter', 'pie', 'donut', 'heatmap', 'stat', 'gauge', 'table', 'geo_map', 'sankey', 'markdown', 'html'];
const STATIC = ['markdown', 'html'];
const download = (name, obj) => {
  const a = h('a', { href: URL.createObjectURL(new Blob([JSON.stringify(obj, null, 2)], { type: 'application/json' })), download: name });
  a.click();
};

export async function dashboardPage(view, { id }) {
  const qp = currentQuery();
  const tv = qp.get('tv') === '1';
  document.body.classList.toggle('tv', tv);
  mount(view, stateBox('Loading…'));
  let dash;
  try {
    dash = (await api.get(`dashboards/${encodeURIComponent(id)}`)).data;
  } catch (e) {
    return mount(view, stateBox(e.message, 'error'));
  }
  // The dashboard's saved range is the default when the link names none.
  if (!qp.get('range') && dash.spec.time.range !== state.range && RANGES[dash.spec.time.range]) {
    state.range = dash.spec.time.range;
    replaceQuery({ range: state.range });
    return refresh();
  }
  const values = {};
  for (const [k, v] of qp) if (k.startsWith('var-')) (values[k.slice(4)] ||= []).push(v);
  const source = {
    data: (panel, qs) => api.get(`dashboards/${id}/panels/${encodeURIComponent(panel)}/data?${qs}`),
    variables: (qs) => api.get(`dashboards/${id}/variables?${qs}`),
  };
  const holder = h('div');
  let current;
  const paint = () => {
    current?.stop();
    current = dashboardView(dash.spec, source, {
      initial: { values, compare: qp.get('compare') || '', refresh: qp.get('refresh') || undefined, onValues: (v) => replaceQuery(Object.fromEntries(Object.keys(v).map((k) => [`var-${k}`, v[k].length === 1 ? v[k][0] : null]))), onControls: (s) => replaceQuery({ compare: s.compare, refresh: s.refresh === dash.spec.refresh ? null : s.refresh }) },
    });
    mount(holder, current.el);
  };
  const msg = h('div');
  const say = (t, kind = '') => mount(msg, t ? h('div', { class: `callout ${kind}` }, t) : null);
  const reloadDash = async () => ((dash = (await api.get(`dashboards/${id}`)).data), paint());

  // ---- share -----------------------------------------------------------------------
  const shareBox = h('div');
  async function share() {
    mount(shareBox, h('div', { class: 'card stack' }, h('h2', null, 'Public links'), stateBox('Loading…')));
    const paintShares = async (newest) => {
      const { data } = await api.get(`dashboards/${id}/shares`);
      const ttl = h('select', { 'aria-label': 'Link lifetime' }, [['24', '1 day'], ['168', '7 days'], ['720', '30 days']].map(([v, l]) => h('option', { value: v }, l)));
      mount(shareBox, h('div', { class: 'card stack' }, h('h2', null, 'Public links'),
        h('p', { class: 'muted' }, 'Anyone with a link can view this dashboard read-only until it expires or you revoke it. Queries are not shown to viewers.'),
        newest ? h('div', { class: 'callout ok' }, 'New link (shown once): ', h('code', null, `${location.origin}${location.pathname}${newest.path}`), ' ', h('button', { type: 'button', onclick: () => copyText(`${location.origin}${location.pathname}${newest.path}`) }, 'Copy')) : null,
        data.length ? data.map((s) => h('div', { class: 'row' }, h('span', { class: `badge ${s.active ? 'ok' : ''}` }, s.revoked ? 'revoked' : s.active ? 'active' : 'expired'), h('span', { class: 'muted' }, `created ${fmtTime(s.created_ms)}, expires ${fmtTime(s.expires_ms)}`), s.active ? h('button', { type: 'button', onclick: async () => (await api.send('DELETE', `dashboards/${id}/shares/${s.id}`), paintShares()) }, 'Revoke') : null)) : h('p', { class: 'muted' }, 'No links yet.'),
        h('div', { class: 'row' }, ttl, h('button', { class: 'primary', type: 'button', onclick: async () => { const r = await api.send('POST', `dashboards/${id}/shares`, { ttl_hours: Number(ttl.value) }); paintShares(r.data); } }, 'Create link')),
        h('button', { type: 'button', class: 'ghost', onclick: () => mount(shareBox) }, 'Close')));
    };
    try {
      await paintShares();
    } catch (e) {
      mount(shareBox, h('div', { class: 'callout warn' }, e.message));
    }
  }

  // ---- edit ------------------------------------------------------------------------
  const editBox = h('div');
  function edit() {
    const draft = structuredClone(dash.spec);
    const title = h('input', { type: 'text', value: dash.title, maxlength: 100, 'aria-label': 'Title' });
    const refreshSel = h('select', { 'aria-label': 'Default refresh' }, ['off', '5s', '10s', '30s', '1m', '5m', '15m', '1h'].map((r) => h('option', { value: r, selected: r === draft.refresh }, r)));
    const rangeSel = h('select', { 'aria-label': 'Default range' }, Object.keys(RANGES).map((r) => h('option', { value: r, selected: r === draft.time.range }, r)));
    const vars = h('textarea', { rows: 6, spellcheck: 'false', 'aria-label': 'Variables as JSON' });
    vars.value = JSON.stringify(draft.variables, null, 2);
    const tabsBox = h('div', { class: 'stack' });
    const paintTabs = () => mount(tabsBox, draft.tabs.map((t, ti) => h('div', { class: 'card stack' },
      h('div', { class: 'row' }, h('input', { type: 'text', value: t.title, 'aria-label': 'Tab title', oninput: (e) => (t.title = e.target.value) }), h('button', { type: 'button', onclick: () => { t.panels.push({ type: 'line', title: 'New panel', w: 6, h: 3, query: { kind: 'sql', text: '' }, options: {} }); paintTabs(); } }, 'Add panel'), draft.tabs.length > 1 ? h('button', { type: 'button', onclick: () => { draft.tabs.splice(ti, 1); paintTabs(); } }, 'Remove tab') : null),
      t.panels.map((p, pi) => {
        const content = STATIC.includes(p.type);
        return h('div', { class: 'card stack editor' },
          h('div', { class: 'row' },
            h('select', { 'aria-label': 'Panel type', onchange: (e) => { p.type = e.target.value; if (STATIC.includes(p.type)) { delete p.query; p.content ||= ''; } else { delete p.content; p.query ||= { kind: 'sql', text: '' }; } paintTabs(); } }, TYPES.map((ty) => h('option', { value: ty, selected: ty === p.type }, ty))),
            h('input', { type: 'text', value: p.title, 'aria-label': 'Panel title', oninput: (e) => (p.title = e.target.value) }),
            h('label', { class: 'muted' }, 'width ', h('input', { type: 'number', min: 1, max: 12, value: p.w || 6, style: 'width:60px', oninput: (e) => (p.w = Number(e.target.value)) })),
            h('label', { class: 'muted' }, 'height ', h('input', { type: 'number', min: 1, max: 8, value: p.h || 3, style: 'width:60px', oninput: (e) => (p.h = Number(e.target.value)) })),
            !content ? h('select', { 'aria-label': 'Query language', onchange: (e) => (p.query.kind = e.target.value) }, ['sql', 'promql'].map((k) => h('option', { value: k, selected: k === p.query.kind }, k === 'sql' ? 'SQL' : 'PromQL'))) : null,
            h('button', { type: 'button', onclick: () => { t.panels.splice(pi, 1); paintTabs(); } }, 'Remove')),
          content ? h('textarea', { rows: 5, spellcheck: 'false', 'aria-label': 'Content', oninput: (e) => (p.content = e.target.value) }, p.content || '') : h('textarea', { rows: 4, spellcheck: 'false', 'aria-label': 'Query', placeholder: 'SELECT ... $__from ... $variable', oninput: (e) => (p.query.text = e.target.value) }, p.query?.text || ''));
      }))));
    paintTabs();
    const save = async () => {
      try {
        draft.variables = JSON.parse(vars.value || '[]');
        draft.refresh = refreshSel.value;
        draft.time = { range: rangeSel.value };
        const r = await api.send('PUT', `dashboards/${id}`, { title: title.value, spec: draft });
        dash = r.data;
        mount(editBox);
        say('Saved.', 'ok');
        paint();
      } catch (e) {
        say(e.message, 'warn');
      }
    };
    mount(editBox, h('div', { class: 'card stack' }, h('h2', null, 'Edit dashboard'),
      h('div', { class: 'row' }, title, h('label', { class: 'muted' }, 'default range ', rangeSel), h('label', { class: 'muted' }, 'default refresh ', refreshSel)),
      h('label', { class: 'muted' }, 'Variables (JSON list)', vars), tabsBox,
      h('div', { class: 'row' }, h('button', { type: 'button', onclick: () => { draft.tabs.push({ title: `Tab ${draft.tabs.length + 1}`, panels: [] }); paintTabs(); } }, 'Add tab'), h('button', { class: 'primary', type: 'button', onclick: save }, 'Save'), h('button', { type: 'button', class: 'ghost', onclick: () => mount(editBox) }, 'Cancel'))));
  }

  const head = h('div', { class: 'page-head' }, !tv ? h('a', { href: href('dashboards') }, '← Dashboards') : null, h('h2', null, dash.title),
    !tv ? h('span', { class: 'row' },
      h('button', { type: 'button', onclick: edit }, 'Edit'), h('button', { type: 'button', onclick: share }, 'Share'),
      h('button', { type: 'button', onclick: async () => download(`${dash.title}.json`, (await api.get(`dashboards/${id}/export`)).data) }, 'Export JSON'),
      h('a', { href: `${href('dashboards', id)}${href('dashboards', id).includes('?') ? '&' : '?'}tv=1` }, 'TV mode'),
      h('button', { type: 'button', class: 'ghost', onclick: async () => { if (confirm(`Delete the dashboard "${dash.title}"?`)) { await api.send('DELETE', `dashboards/${id}`); location.hash = href('dashboards'); } } }, 'Delete')) : h('a', { href: href('dashboards', id) }, 'Exit TV mode'));
  mount(view, head, msg, editBox, shareBox, holder);
  paint();
}
