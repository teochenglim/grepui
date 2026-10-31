// A dashboard: variable bar, refresh and compare controls, tabs and a grid of panels. The same
// code draws a private dashboard (editable) and a public one (read-only); `source` says where
// panel data and variable options come from.

import { timeWindow } from '../state.js';
import { h, mount } from '../util/dom.js';
import { fmtNum } from '../util/format.js';
import { renderPanel } from './panel.js';
import { stateBox } from './table.js';

const REFRESH = ['off', '5s', '10s', '30s', '1m', '5m', '15m', '1h'];
const MS = { off: 0, '5s': 5e3, '10s': 1e4, '30s': 3e4, '1m': 6e4, '5m': 3e5, '15m': 9e5, '1h': 36e5 };
const COMPARE = [['', 'No comparison'], ['1h', 'vs 1 hour ago'], ['1d', 'vs 1 day ago'], ['7d', 'vs 7 days ago'], ['prev', 'vs previous period']];

/**
 * spec: the dashboard spec. source: { data(panelId, qs) -> Promise<{data}>, variables(qs) -> Promise<{data}> }.
 * Returns { el, stop, reload, state }.
 */
export function dashboardView(spec, source, { onSpecChange, initial = {} } = {}) {
  const st = { tab: 0, refresh: initial.refresh || spec.refresh || 'off', compare: initial.compare || '', values: { ...(initial.values || {}) }, timer: null, epoch: 0 };
  const bar = h('div', { class: 'dash-bar' });
  const tabs = h('div', { class: 'tabs' });
  const grid = h('div', { class: 'dash-grid' });
  const el = h('div', { class: 'stack' }, bar, tabs, grid);
  let options = [];

  const query = () => {
    const w = timeWindow();
    const p = new URLSearchParams({ from: String(w.from), to: String(w.to) });
    if (st.compare) p.set('compare', st.compare);
    for (const [k, vs] of Object.entries(st.values)) for (const v of vs) p.append(`var-${k}`, v);
    return p.toString();
  };
  const stop = () => st.timer && (clearInterval(st.timer), (st.timer = null));
  const schedule = () => {
    stop();
    const ms = MS[st.refresh] || 0;
    if (ms >= 5000) st.timer = setInterval(() => (el.isConnected ? loadPanels() : stop()), ms);
  };

  async function loadPanel(p, body, mine) {
    try {
      const { data } = await source.data(p.id, query());
      if (mine !== st.epoch) return;
      mount(body, renderPanel(data, p.options || {}));
    } catch (e) {
      if (mine === st.epoch) mount(body, h('div', { class: 'callout warn' }, e.message));
    }
  }

  function loadPanels() {
    const mine = ++st.epoch;
    for (const [p, body] of panelBodies) if (p.id) loadPanel(p, body, mine);
  }

  let panelBodies = [];
  function paintGrid() {
    const tab = spec.tabs[st.tab] || spec.tabs[0];
    panelBodies = [];
    mount(grid, !tab.panels.length ? h('div', { class: 'empty', style: 'grid-column: 1 / -1' }, 'This tab has no panels yet.') : tab.panels.map((p) => {
      const body = h('div', { class: 'panel-body', style: `min-height:${(p.h || 3) * 50}px` }, p.id ? stateBox('Loading…') : h('div', { class: 'muted' }, 'Save the dashboard to see this panel.'));
      panelBodies.push([p, body]);
      return h('div', { class: 'card', style: `grid-column: span ${p.w || 6}` }, h('div', { class: 'panel-title' }, h('h2', null, p.title)), body);
    }));
    loadPanels();
  }

  function paintTabs() {
    mount(tabs, spec.tabs.length > 1 ? spec.tabs.map((t, i) => h('button', { type: 'button', class: `tab${i === st.tab ? ' active' : ''}`, onclick: () => ((st.tab = i), paintTabs(), paintGrid()) }, t.title)) : null);
  }

  function varControl(v) {
    const sel = h('select', { 'aria-label': v.label || v.name, multiple: v.multi || null });
    // A multi-select with nothing chosen means every value; a single select starts at its first.
    const given = st.values[v.name] || (v.default?.length ? v.default : null);
    const chosen = new Set(given || (v.multi ? v.options : []));
    if (v.multi && !given) st.values[v.name] = [...v.options];
    for (const o of v.options) sel.append(h('option', { value: o, selected: chosen.has(o) || null }, o));
    if (!v.multi && !chosen.size && v.options.length) st.values[v.name] = [v.options[0]];
    sel.addEventListener('change', () => {
      st.values[v.name] = [...sel.selectedOptions].map((o) => o.value);
      initial.onValues?.(st.values);
      loadPanels();
    });
    return h('label', { class: 'varbox' }, h('span', { class: 'muted' }, v.label || v.name), sel, v.error ? h('span', { class: 'err', title: v.error }, '!') : null);
  }

  async function paintBar() {
    const refreshSel = h('select', { 'aria-label': 'Auto refresh' }, REFRESH.map((r) => h('option', { value: r, selected: r === st.refresh }, r === 'off' ? 'Refresh: off' : `Every ${r}`)));
    refreshSel.addEventListener('change', () => ((st.refresh = refreshSel.value), schedule(), initial.onControls?.(st)));
    const cmpSel = h('select', { 'aria-label': 'Compare with the past' }, COMPARE.map(([v, l]) => h('option', { value: v, selected: v === st.compare }, l)));
    cmpSel.addEventListener('change', () => ((st.compare = cmpSel.value), initial.onControls?.(st), loadPanels()));
    const now = h('button', { type: 'button', onclick: loadPanels }, 'Refresh now');
    const varsBox = h('span', { class: 'row' });
    mount(bar, varsBox, h('span', { class: 'spacer grow' }), cmpSel, refreshSel, now);
    if (spec.variables.length) {
      try {
        const { data } = await source.variables(query());
        options = data;
        mount(varsBox, options.map(varControl));
      } catch (e) {
        mount(varsBox, h('span', { class: 'err' }, e.message));
      }
    }
  }

  async function show() {
    paintTabs();
    await paintBar();
    paintGrid();
    schedule();
  }
  void fmtNum;
  void onSpecChange;
  show();
  return { el, stop, reload: loadPanels, show, state: st };
}
