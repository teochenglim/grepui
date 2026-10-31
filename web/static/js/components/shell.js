// The app frame: sidebar, top bar (org, range, Show SQL), content outlet.

import { href } from '../router.js';
import { h, mount } from '../util/dom.js';
import { orgSwitcher } from './orgswitcher.js';
import { sqlDrawer } from './sqldrawer.js';
import { roleIn, session, state } from '../state.js';
import { timeRange } from './timerange.js';

// Pages that exist now, and the release that adds each of the others (RELEASE.md).
const LIVE = [
  ['logs', 'Logs'],
  ['traces', 'Traces'],
  ['llm', 'LLM'],
  ['dashboards', 'Dashboards'],
  ['alerts', 'Alerts'],
  ['metrics', 'Metrics'],
  ['rum', 'RUM'],
  ['services', 'Services'],
  ['service-map', 'Service map'],
  ['catalog', 'Catalog'],
  ['streams', 'Streams'],
  ['health', 'Data health'],
  ['rollups', 'Rollups'],
  ['pipelines', 'Pipelines'],
  ['functions', 'Functions'],
  ['enrichment', 'Enrichment'],
  ['redaction', 'Redaction'],
  ['retention', 'Retention'],
  ['connect', 'Connect'],
];
const SOON = [];

/** Who is signed in, with sign-out (R-C1). With --no-auth there is nobody to sign out. */
function whoMenu() {
  const me = session.me;
  if (!me) return h('span');
  if (me.no_auth) return h('span', { class: 'badge warn', title: 'Started with --no-auth: everyone is an admin and nothing is audited' }, 'no sign-in');
  if (!me.authenticated) return h('span');
  const role = roleIn(state.org);
  return h('span', { class: 'who' }, h('span', { title: me.platform_admin ? 'platform admin' : 'role in this org' }, `${me.username}${role ? ` · ${role}` : ''}`),
    me.kind === 'user' && me.user?.source === 'local' ? h('button', { type: 'button', class: 'ghost', onclick: async () => {
      const current = prompt('Current password'); if (!current) return;
      const next = prompt('New password (10 characters or more)'); if (!next) return;
      try {
        const r = await fetch('/api/v1/auth/password', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ current, new: next }) });
        const j = await r.json();
        alert(r.ok ? 'Password changed.' : j.error);
      } catch (e) { alert(e.message); }
    } }, 'Password') : null,
    h('button', { type: 'button', class: 'ghost', onclick: async () => { await fetch('/api/v1/auth/logout', { method: 'POST' }); location.hash = ''; location.reload(); } }, 'Sign out'));
}

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
      h('div', { class: 'brand', id: 'brand' }, h('span', { class: 'logo' }, 'g'), 'grepui'),
      h('h3', null, 'Data'),
      LIVE.map(([k, label]) => h('a', { class: `nav${cur === k ? ' active' : ''}`, href: href(k) }, label)),
      roleIn(state.org) === 'admin' || session.me?.platform_admin ? h('h3', null, 'Manage') : null,
      roleIn(state.org) === 'admin' ? h('a', { class: `nav${cur === 'admin' ? ' active' : ''}`, href: href('admin') }, 'Administration') : null,
      roleIn(state.org) === 'admin' ? h('a', { class: `nav${cur === 'reports' ? ' active' : ''}`, href: href('reports') }, 'Reports') : null,
      session.me?.platform_admin && !session.me?.no_auth ? h('a', { class: `nav${cur === 'platform' ? ' active' : ''}`, href: href('platform') }, 'Platform') : null,
      SOON.length ? h('h3', null, 'Coming') : null,
      SOON.map(([label, v]) => h('span', { class: 'nav soon', title: `Planned for ${v}` }, label, h('span', { class: 'badge' }, v))),
    );
  };

  mount(root, h('div', { class: 'shell' }, nav, h('div', { class: 'main' }, h('header', { class: 'top' }, h('span', { class: 'spacer' }), orgSwitcher(), rangeSel, toggle, whoMenu()), view)), drawer);
  return { view, paintNav };
}
