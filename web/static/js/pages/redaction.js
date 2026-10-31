// Redaction (R-N4): masks applied to every response and export, per field and role.

import * as api from '../api.js';
import { dataTable, stateBox } from '../components/table.js';
import { h, mount } from '../util/dom.js';

export async function redactionPage(view) {
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  const msg = h('div');
  const say = (t, kind = '') => mount(msg, t ? h('div', { class: `callout ${kind}` }, t) : null);
  mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Redaction'), h('span', { class: 'sub' }, 'hide sensitive fields in responses and exports')), msg, out);
  const field = h('input', { placeholder: 'field, e.g. password', 'aria-label': 'Field' });
  const mask = h('select', { 'aria-label': 'Mask' }, [['full', 'full: [redacted]'], ['partial', 'partial: keep the last 4'], ['hash', 'hash: 8 hex, to correlate']].map(([v, l]) => h('option', { value: v }, l)));
  const exempt = h('input', { type: 'checkbox', checked: true, 'aria-label': 'Admins see the value' });
  async function load() {
    try {
      const { data } = await api.get('redaction');
      const act = (fn) => fn().then(load).catch((e) => say(e.message, 'warn'));
      mount(out,
        h('div', { class: 'callout' }, `Rules match a field by name anywhere in a response, including inside JSON attributes and dotted keys (http.request.header.authorization matches "authorization"), and a CSV column of that name. Until access control (v0.11) there are two roles: admin (grepui started with --admin-writes) and viewer (everyone else, and always public dashboard links). You are ${data.admin_writes ? 'an admin' : 'a viewer'} here. A query that renames a column, or text inside a log body, is not recognised.`),
        data.admin_writes ? null : h('div', { class: 'callout warn' }, 'Changing rules needs admin writes: start grepui with --admin-writes.'),
        dataTable({
          columns: [
            { key: 'field', label: 'Field' }, { key: 'mask', label: 'Mask' },
            { key: 'e', label: 'Admins see it', render: (r) => (r.exempt_roles.includes('admin') ? 'yes' : 'no') },
            { key: 'x', label: '', render: (r) => h('button', { type: 'button', class: 'ghost', disabled: !data.admin_writes, onclick: () => act(() => api.send('DELETE', `redaction/${r.id}`)) }, 'Delete') },
          ],
          rows: data.rules,
          empty: 'No rules: nothing is masked.',
        }),
        h('div', { class: 'card row' }, field, mask, h('label', { class: 'row' }, exempt, 'admins see the value'),
          h('button', { class: 'primary', type: 'button', disabled: !data.admin_writes, onclick: () => act(async () => { await api.send('POST', 'redaction', { field: field.value, mask: mask.value, exempt_roles: exempt.checked ? ['admin'] : [] }); field.value = ''; say('Rule added.', 'ok'); }) }, 'Add rule')));
    } catch (e) {
      mount(out, stateBox(e.message, 'error'));
    }
  }
  load();
}
