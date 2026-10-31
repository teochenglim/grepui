// Alerts: rules, history, incidents, destinations and downtimes (R-A1 – R-A8).

import * as api from '../api.js';
import { dataTable, stateBox } from '../components/table.js';
import { href } from '../router.js';
import { h, mount } from '../util/dom.js';
import { fmtAge, fmtTime } from '../util/format.js';

const TABS = [['rules', 'Rules'], ['history', 'History'], ['incidents', 'Incidents'], ['destinations', 'Destinations'], ['downtimes', 'Downtimes']];
const hint = (e) => (/migrate/.test(e.message) ? 'The state store needs `grepui migrate` (schema version 4) before alerts can be used.' : e.message);
const tone = (s) => (s === 'firing' || s === 'error' ? 'bad' : s === 'ok' ? 'ok' : '');
const save = (name, obj) => h('a', { href: URL.createObjectURL(new Blob([JSON.stringify(obj, null, 2)], { type: 'application/json' })), download: name }).click();

export async function alertsPage(view, params, tab = 'rules') {
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  const msg = h('div');
  const say = (t, kind = '') => mount(msg, t ? h('div', { class: `callout ${kind}` }, t) : null);
  const act = async (fn) => {
    try {
      await fn();
      await load();
    } catch (e) {
      say(hint(e), 'warn');
    }
  };
  const tabs = h('div', { class: 'tabs', role: 'tablist' }, TABS.map(([k, l]) => h('button', { type: 'button', role: 'tab', class: `tab${k === tab ? ' active' : ''}`, 'aria-selected': k === tab, onclick: () => alertsPage(view, params, k) }, l)));
  mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Alerts'), h('span', { class: 'sub' }, 'rules, incidents and where notifications go')), tabs, msg, out);

  const views = {
    async rules() {
      const [{ data: rules }, { data: info }] = await Promise.all([api.get('alerts'), api.get('alerts/info')]);
      const folders = [...new Set(rules.map((r) => r.spec.folder || ''))];
      const importer = h('textarea', { rows: 5, spellcheck: 'false', placeholder: 'Paste exported alerts as JSON', 'aria-label': 'Import JSON' });
      mount(out,
        h('div', { class: 'row' },
          h('a', { class: 'primary btn', href: href('alerts', 'new') }, 'New alert'),
          h('button', { type: 'button', onclick: async () => save('grepui-alerts.json', (await api.get('alerts/export')).data) }, 'Export all'),
          h('span', { class: 'muted' }, info.scheduler ? `scheduler on, ticks every ${info.tick_s}s, leases ${info.lease_s}s` : 'scheduler off in this process (--no-alerts)')),
        folders.map((f) => h('div', { class: 'card stack' }, f ? h('h2', null, `📁 ${f}`) : null,
          dataTable({
            columns: [
              { key: 'name', label: 'Alert', render: (r) => h('a', { href: href('alerts', r.id) }, r.name) },
              { key: 'kind', label: 'Kind', render: (r) => r.spec.kind },
              { key: 'state', label: 'State', render: (r) => h('span', { class: `badge ${tone(r.status.state)}` }, r.spec.enabled ? r.status.state || 'not run yet' : 'disabled') },
              { key: 'v', label: 'Last value', num: true, render: (r) => (r.status.last_value ?? '–') },
              { key: 'e', label: 'Evaluated', render: (r) => (r.status.last_eval_ms ? fmtAge(r.status.last_eval_ms) : '–') },
              { key: 'l', label: 'Leader', render: (r) => (r.status.leader ? r.status.leader.slice(0, 6) : '–') },
              { key: 'x', label: '', render: (r) => h('button', { type: 'button', class: 'ghost', onclick: () => act(async () => { const p = await api.get(`alerts/${r.id}/preview`); say(`Preview of "${r.name}": ${p.data.state}, ${p.data.samples.length} series, nothing was sent.`, 'ok'); }) }, 'Preview') },
            ],
            rows: rules.filter((r) => (r.spec.folder || '') === f),
            empty: 'No alerts yet.',
          }))),
        rules.length ? null : h('div', { class: 'card' }, 'No alerts yet. Create one, or import a bundle below.'),
        h('div', { class: 'card stack' }, h('h2', null, 'Import'), importer,
          h('div', { class: 'row' }, h('button', { type: 'button', onclick: () => act(async () => { const r = await api.send('POST', 'alerts/import', JSON.parse(importer.value)); importer.value = ''; say(`Imported ${r.data.imported.length}, skipped ${r.data.skipped.length} that already exist. ${r.data.notes.join(' ')}`, 'ok'); }) }, 'Import'))));
    },
    async history() {
      const { data } = await api.get('alerts/history', { limit: 200 });
      mount(out, dataTable({
        columns: [
          { key: 'at_ms', label: 'When', render: (r) => fmtTime(r.at_ms) },
          { key: 'alert_id', label: 'Alert', render: (r) => h('a', { href: href('alerts', r.alert_id) }, r.alert_id.slice(0, 8)) },
          { key: 'state', label: 'State', render: (r) => h('span', { class: `badge ${tone(r.state)}` }, r.state) },
          { key: 'outcome', label: 'Outcome' },
          { key: 'detail', label: 'Detail', render: (r) => h('code', { class: 'muted' }, (r.detail?.error || (r.detail?.samples || []).map((s) => s.value).join(', ') || '').toString().slice(0, 80)) },
        ],
        rows: data,
        empty: 'Nothing has been evaluated yet.',
      }));
    },
    async incidents() {
      const { data } = await api.get('incidents');
      const step = (id, verb) => act(async () => api.send('POST', `incidents/${id}/${verb}`, { note: prompt('Note (optional)') || '' }));
      mount(out, data.length ? data.map((i) => h('div', { class: 'card stack' },
        h('div', { class: 'row' }, h('strong', { class: 'grow' }, i.spec.title), h('span', { class: `badge ${i.spec.state === 'resolved' ? 'ok' : 'bad'}` }, i.spec.state), h('span', { class: 'muted' }, `opened ${fmtAge(i.spec.created_ms)}`),
          i.spec.state === 'open' ? h('button', { type: 'button', onclick: () => step(i.id, 'ack') }, 'Acknowledge') : null,
          i.spec.state !== 'resolved' ? h('button', { type: 'button', onclick: () => step(i.id, 'resolve') }, 'Resolve') : null),
        h('ol', { class: 'timeline' }, i.spec.timeline.map((e) => h('li', null, `${fmtTime(e.ms)} · ${e.event} by ${e.by}${e.note ? ` — ${e.note}` : ''}`))))) : stateBox('No incidents.'));
    },
    async destinations() {
      const [{ data }, { data: info }] = await Promise.all([api.get('destinations'), api.get('alerts/info')]);
      const kind = h('select', { 'aria-label': 'Kind' }, h('option', { value: 'webhook' }, 'Webhook (http)'), h('option', { value: 'email' }, 'Email'));
      const name = h('input', { placeholder: 'Name', 'aria-label': 'Name' });
      const target = h('input', { class: 'grow', placeholder: 'http://host:port/path, or a@b.com, c@d.com', 'aria-label': 'URL or recipients' });
      mount(out,
        info.email ? null : h('div', { class: 'callout' }, 'Email needs an SMTP relay: start grepui with --smtp host:port and --smtp-from. Webhooks are plain http (no TLS client); put a relay in front for https.'),
        data.map((d) => h('div', { class: 'card row' }, h('strong', null, d.name), h('span', { class: 'badge' }, d.spec.kind), h('code', { class: 'grow muted' }, d.spec.url || d.spec.to.join(', ')),
          h('button', { type: 'button', onclick: async () => { const r = await api.send('POST', `destinations/${d.id}/test`); say(r.data.ok ? `Test sent to "${d.name}".` : `Test to "${d.name}" failed: ${r.data.error}`, r.data.ok ? 'ok' : 'warn'); } }, 'Send test'),
          h('button', { type: 'button', class: 'ghost', onclick: () => confirm(`Delete "${d.name}"?`) && act(() => api.send('DELETE', `destinations/${d.id}`)) }, 'Delete'))),
        h('div', { class: 'card row' }, name, kind, target, h('button', { class: 'primary', type: 'button', onclick: () => act(async () => { const body = kind.value === 'webhook' ? { name: name.value, kind: 'webhook', url: target.value } : { name: name.value, kind: 'email', to: target.value.split(',').map((x) => x.trim()).filter(Boolean) }; await api.send('POST', 'destinations', body); say('Destination added.', 'ok'); }) }, 'Add')));
    },
    async downtimes() {
      const [{ data }, { data: rules }] = await Promise.all([api.get('downtimes'), api.get('alerts')]);
      const name = h('input', { placeholder: 'Name', 'aria-label': 'Name' });
      const mins = h('input', { type: 'number', value: '60', min: '1', 'aria-label': 'Minutes', style: 'width:90px' });
      const which = h('select', { 'aria-label': 'Alert' }, h('option', { value: '' }, 'All alerts'), rules.map((r) => h('option', { value: r.id }, r.name)));
      mount(out,
        data.length ? data.map((d) => h('div', { class: 'card row' }, h('strong', null, d.name), h('span', { class: 'muted grow' }, `${fmtTime(d.spec.from_ms)} → ${fmtTime(d.spec.to_ms)} · ${d.spec.alerts.length ? `${d.spec.alerts.length} alert(s)` : 'all alerts'}`),
          h('button', { type: 'button', class: 'ghost', onclick: () => act(() => api.send('DELETE', `downtimes/${d.id}`)) }, 'Delete'))) : h('div', { class: 'card muted' }, 'No downtimes.'),
        h('div', { class: 'card row' }, name, which, 'for', mins, 'minutes from now', h('button', { class: 'primary', type: 'button', onclick: () => act(async () => { const now = Date.now(); await api.send('POST', 'downtimes', { name: name.value, from_ms: now, to_ms: now + Number(mins.value) * 60000, alerts: which.value ? [which.value] : [] }); say('Downtime added.', 'ok'); }) }, 'Silence')));
    },
  };

  async function load() {
    try {
      await views[tab]();
    } catch (e) {
      mount(out, stateBox(hint(e), 'error'));
    }
  }
  load();
}
