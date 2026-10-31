// A public dashboard: read-only, opened from a link. No editing, no queries, no navigation.

import * as api from '../api.js';
import { dashboardView } from '../components/dashview.js';
import { stateBox } from '../components/table.js';
import { currentQuery } from '../router.js';
import { RANGES, state } from '../state.js';
import { h, mount } from '../util/dom.js';

export async function publicPage(view, { token }) {
  document.body.classList.add('tv');
  mount(view, stateBox('Loading…'));
  let d;
  try {
    d = (await api.get(`public/${encodeURIComponent(token)}`)).data;
  } catch (e) {
    return mount(view, h('div', { class: 'callout warn' }, e.message));
  }
  if (RANGES[d.spec.time.range] && !currentQuery().get('range')) state.range = d.spec.time.range;
  const values = {};
  for (const [k, v] of currentQuery()) if (k.startsWith('var-')) (values[k.slice(4)] ||= []).push(v);
  const source = {
    data: (panel, qs) => api.get(`public/${token}/panels/${encodeURIComponent(panel)}/data?${qs}`),
    variables: (qs) => api.get(`public/${token}/variables?${qs}`),
  };
  mount(view, h('div', { class: 'page-head' }, h('h2', null, d.title), h('span', { class: 'sub' }, 'read-only')), dashboardView(d.spec, source, { initial: { values } }).el);
}
