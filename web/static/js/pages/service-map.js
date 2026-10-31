// The service map: who calls whom, with call volume and error rate on every edge.

import * as api from '../api.js';
import { serviceMap } from '../components/servicemap.js';
import { stateBox } from '../components/table.js';
import { currentQuery, replaceQuery, refresh } from '../router.js';
import { timeWindow } from '../state.js';
import { h, mount } from '../util/dom.js';
import { serviceHref } from './services.js';

export async function serviceMapPage(view) {
  const loops = currentQuery().get('loops') === '1';
  const box = h('div', { class: 'card smap-wrap' }, stateBox('Loading…'));
  const toggle = h('input', { type: 'checkbox', checked: loops, onchange: () => (replaceQuery({ loops: toggle.checked ? '1' : null }), refresh()) });
  mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Service map'), h('span', { class: 'sub' }, 'caller to callee, from parent and child spans'), h('label', { class: 'check' }, toggle, 'Show calls within a service')), box);
  const w = timeWindow();
  try {
    const { data } = await api.get('dependencies', { from: w.from, to: w.to, self_loops: loops ? true : null });
    mount(box, data.nodes.length ? serviceMap(data, (name) => (location.hash = serviceHref(name))) : h('div', { class: 'empty' }, 'No calls between services in this range. Widen the time range, or check that spans carry parent ids.'));
  } catch (e) {
    mount(box, stateBox(e.message, 'error'));
  }
}
