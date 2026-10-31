// Platform administration (R-C6): orgs, users and the platform audit trail. Platform admins only.

import * as api from '../api.js';
import { dataTable, stateBox } from '../components/table.js';
import { h, mount } from '../util/dom.js';
import { fmtTime } from '../util/format.js';

const TABS = [['orgs', 'Orgs'], ['users', 'Users'], ['audit', 'Audit']];
const root = (path, opts) => fetch(`/api/v1/platform/${path}`, { ...opts, headers: { 'Content-Type': 'application/json', Accept: 'application/json' } }).then(async (r) => {
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j.error || `HTTP ${r.status}`);
  return j;
});

export async function platformPage(view, _p, tab = 'orgs') {
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  const msg = h('div');
  const say = (t, kind = '') => mount(msg, t ? h('div', { class: `callout ${kind}` }, t) : null);
  const act = async (fn) => { try { await fn(); await load(); } catch (e) { say(e.message, 'warn'); } };
  mount(view,
    h('div', { class: 'page-head' }, h('h2', null, 'Platform'), h('span', { class: 'sub' }, 'orgs and people across grepui')),
    h('div', { class: 'tabs', role: 'tablist' }, TABS.map(([k, l]) => h('button', { type: 'button', role: 'tab', class: 'tab', 'aria-selected': k === tab, onclick: () => platformPage(view, _p, k) }, l))), msg, out);
  const views = {
    async orgs() {
      const { data } = await root('orgs');
      const f = { name: h('input', { placeholder: 'org name (a-z, 0-9, - _)', 'aria-label': 'Org name' }), db: h('input', { placeholder: 'database (default org_<name>)', 'aria-label': 'Database' }), create: h('input', { type: 'checkbox', checked: true, 'aria-label': 'Create the database' }), admin: h('input', { placeholder: 'org admin (existing user)', 'aria-label': 'Org admin' }) };
      mount(out,
        dataTable({
          columns: [{ key: 'name', label: 'Org' }, { key: 'database', label: 'Database' }, { key: 'traces_table', label: 'Traces table' }, { key: 'logs_table', label: 'Logs table' },
            { key: 'x', label: '', render: (o) => (o.name === 'default' ? h('span', { class: 'muted' }, 'from the flags') : h('button', { type: 'button', class: 'ghost', onclick: () => confirm(`Delete org "${o.name}"? Its saved views, dashboards, alerts, tokens and memberships are removed. Its telemetry database is not touched.`) && act(async () => { const r = await root(`orgs/${o.name}`, { method: 'DELETE' }); say(`Deleted ${o.name}: ${JSON.stringify(r.data.removed)}. Telemetry untouched.`, 'ok'); }) }, 'Delete')) }],
          rows: data, empty: 'No orgs.',
        }),
        h('div', { class: 'card row' }, f.name, f.db, h('label', { class: 'row' }, f.create, 'create the database'), f.admin,
          h('button', { class: 'primary', type: 'button', onclick: () => act(async () => { await root('orgs', { method: 'POST', body: JSON.stringify({ name: f.name.value, database: f.db.value || undefined, create_database: f.create.checked, admin: f.admin.value || undefined }) }); say('Org created. Point your collectors at its database.', 'ok'); }) }, 'Create org')));
    },
    async users() {
      const { data } = await root('users');
      const f = { name: h('input', { placeholder: 'username', 'aria-label': 'Username' }), pw: h('input', { type: 'password', placeholder: 'password', 'aria-label': 'Password', autocomplete: 'new-password' }), admin: h('input', { type: 'checkbox', 'aria-label': 'Platform admin' }) };
      mount(out,
        dataTable({
          columns: [{ key: 'username', label: 'User' }, { key: 'source', label: 'Account' },
            { key: 'platform_admin', label: 'Platform admin', render: (u) => (u.platform_admin ? 'yes' : '') },
            { key: 'orgs', label: 'Orgs', render: (u) => Object.entries(u.orgs).map(([o, r]) => `${o}: ${r}`).join(', ') || '–' },
            { key: 'disabled', label: 'Status', render: (u) => h('span', { class: `badge ${u.disabled ? 'bad' : 'ok'}` }, u.disabled ? 'disabled' : 'active') },
            { key: 'x', label: '', render: (u) => h('span', { class: 'row' },
              h('button', { type: 'button', class: 'ghost', onclick: () => act(() => root(`users/${u.username}`, { method: 'PUT', body: JSON.stringify({ disabled: !u.disabled }) })) }, u.disabled ? 'Enable' : 'Disable'),
              h('button', { type: 'button', class: 'ghost', onclick: () => { const p = prompt(`New password for ${u.username}`); p && act(() => root(`users/${u.username}`, { method: 'PUT', body: JSON.stringify({ password: p }) })); } }, 'Set password'),
              h('button', { type: 'button', class: 'ghost', onclick: () => confirm(`Delete ${u.username}?`) && act(() => root(`users/${u.username}`, { method: 'DELETE' })) }, 'Delete')) }],
          rows: data, empty: 'No users.',
        }),
        h('div', { class: 'card row' }, f.name, f.pw, h('label', { class: 'row' }, f.admin, 'platform admin'),
          h('button', { class: 'primary', type: 'button', onclick: () => act(async () => { await root('users', { method: 'POST', body: JSON.stringify({ username: f.name.value, password: f.pw.value, platform_admin: f.admin.checked }) }); say('User created. Add them to an org under Administration → Members.', 'ok'); }) }, 'Create user')));
    },
    async audit() {
      const { data } = await root('audit?limit=200');
      mount(out, dataTable({
        columns: [{ key: 'at_ms', label: 'When', render: (r) => fmtTime(r.at_ms) }, { key: 'username', label: 'Who' }, { key: 'action', label: 'What' }, { key: 'detail', label: 'Detail' },
          { key: 'outcome', label: 'Outcome', render: (r) => h('span', { class: `badge ${r.outcome === 'ok' ? 'ok' : r.outcome === 'denied' ? 'bad' : 'warn'}` }, r.outcome) }],
        rows: data, empty: 'Nothing yet.',
      }));
    },
  };
  async function load() {
    try { say(''); await views[tab](); } catch (e) { mount(out, stateBox(e.message, 'error')); }
  }
  load();
}
