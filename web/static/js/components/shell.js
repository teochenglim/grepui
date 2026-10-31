// The app frame: sidebar, top bar (org, range, Show SQL), content outlet.

import { href } from '../router.js';
import { h, mount } from '../util/dom.js';
import { orgSwitcher } from './orgswitcher.js';
import { sqlDrawer } from './sqldrawer.js';
import { state } from '../state.js';
import { timeRange } from './timerange.js';

// Pages that exist now, and the release that adds each of the others (RELEASE.md).
const LIVE = [
  ['logs', 'Logs'],
  ['traces', 'Traces'],
  ['streams', 'Streams'],
  ['health', 'Data health'],
  ['connect', 'Connect'],
];
const SOON = [
  ['Services', 'v0.5'], ['Metrics', 'v0.6'], ['Dashboards', 'v0.8'], ['Alerts', 'v0.9'],
];

export function buildShell(root) {
  const { toggle, drawer } = sqlDrawer();
  const rangeSel = timeRange();
  const view = h('main', { class: 'view', id: 'view' });
  const nav = h('nav', { class: 'side', 'aria-label': 'Main' });

  const paintNav = (parts) => {
    rangeSel.value = state.range; // the URL may have changed the range
    const cur = parts[1] || 'streams';
    mount(
      nav,
      h('div', { class: 'brand' }, h('span', { class: 'logo' }, 'g'), 'grepui'),
      h('h3', null, 'Data'),
      LIVE.map(([k, label]) => h('a', { class: `nav${cur === k ? ' active' : ''}`, href: href(k) }, label)),
      h('h3', null, 'Coming'),
      SOON.map(([label, v]) => h('span', { class: 'nav soon', title: `Planned for ${v}` }, label, h('span', { class: 'badge' }, v))),
    );
  };

  mount(root, h('div', { class: 'shell' }, nav, h('div', { class: 'main' }, h('header', { class: 'top' }, h('span', { class: 'spacer' }), orgSwitcher(), rangeSel, toggle), view)), drawer);
  return { view, paintNav };
}
