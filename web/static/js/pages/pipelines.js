// Pipelines (R-N1): GreptimeDB ingest pipelines of this org: list, versions, dry-run, save, delete.

import * as api from '../api.js';
import { dataTable, stateBox } from '../components/table.js';
import { h, mount } from '../util/dom.js';
import { fmtAge } from '../util/format.js';

const SAMPLE_YAML = `version: 2
processors:
  - dissect:
      fields:
        - message
      patterns:
        - '%{ip} - %{method} %{path} %{status}'
      ignore_missing: true
  - date:
      fields:
        - time
      formats:
        - "%Y-%m-%d %H:%M:%S"
      ignore_missing: true
transform:
  - fields:
      - ip
      - method
      - path
    type: string
  - fields:
      - status
    type: int32
  - fields:
      - time
    type: time
    index: timestamp
`;
const SAMPLE_DATA = '[{"message": "10.0.0.1 - GET /cart 200", "time": "2026-10-31 07:00:00"}]';

export async function pipelinesPage(view) {
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  const msg = h('div');
  const say = (t, kind = '') => mount(msg, t ? h('div', { class: `callout ${kind}` }, t) : null);
  mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Pipelines'), h('span', { class: 'sub' }, 'transform data at ingest: GreptimeDB pipelines (YAML), with a dry-run')), msg, out);
  const name = h('input', { placeholder: 'name (a-z, 0-9, _)', 'aria-label': 'Pipeline name' });
  const yaml = h('textarea', { rows: 16, spellcheck: 'false', 'aria-label': 'Pipeline YAML' }, SAMPLE_YAML);
  const data = h('textarea', { rows: 4, spellcheck: 'false', 'aria-label': 'Sample records (JSON array)' }, SAMPLE_DATA);
  const result = h('div');

  async function load() {
    try {
      const { data: d } = await api.get('pipelines');
      const act = async (fn) => { try { await fn(); await load(); } catch (e) { say(e.message, 'warn'); } };
      mount(out,
        d.admin_writes ? null : h('div', { class: 'callout warn' }, 'Saving and deleting pipelines needs admin writes: start grepui with --admin-writes. Dry-run works without it.'),
        dataTable({
          columns: [
            { key: 'name', label: 'Pipeline', render: (p) => h('a', { href: '#', onclick: async (e) => { e.preventDefault(); const r = await api.get(`pipelines/${p.name}`); name.value = p.name; yaml.value = r.data.yaml; say(`Loaded ${p.name} (${r.data.version}).`, 'ok'); } }, p.name) },
            { key: 'full_name', label: 'Use at ingest as' },
            { key: 'v', label: 'Versions', num: true, render: (p) => p.versions.length },
            { key: 'l', label: 'Latest', render: (p) => (p.latest_ms ? fmtAge(p.latest_ms) : '–') },
            { key: 'x', label: '', render: (p) => h('button', { type: 'button', class: 'ghost', disabled: !d.admin_writes, onclick: () => confirm(`Delete every version of "${p.name}"?`) && act(async () => { await api.send('DELETE', `pipelines/${p.name}`); say('Deleted.', 'ok'); }) }, 'Delete') },
          ],
          rows: d.pipelines,
          empty: 'No pipelines yet.',
        }),
        h('div', { class: 'card stack' }, h('h2', null, 'Edit'), h('div', { class: 'row' }, name), yaml,
          h('label', { class: 'field' }, h('span', null, 'Sample records'), data),
          h('div', { class: 'row' },
            h('button', { type: 'button', onclick: async () => {
              try {
                const r = await api.send('POST', 'pipelines/dry-run', { yaml: yaml.value, data: JSON.parse(data.value) });
                say('');
                mount(result, h('div', { class: 'card' }, h('h2', null, 'Dry-run result'), dataTable({ columns: r.data.columns.map((c) => ({ key: c.name, label: `${c.name} (${String(c.type).toLowerCase()})` })), rows: r.data.rows })));
              } catch (e) { mount(result); say(e.message, 'warn'); }
            } }, 'Dry-run'),
            h('button', { class: 'primary', type: 'button', disabled: !d.admin_writes, onclick: () => act(async () => { const r = await api.send('PUT', `pipelines/${name.value}`, { yaml: yaml.value }); say(`Saved as a new version (${r.data.version}). Send data with the header x-greptime-pipeline-name: default.${name.value}.`, 'ok'); }) }, 'Save as new version')),
          result));
    } catch (e) {
      mount(out, stateBox(e.message, 'error'));
    }
  }
  load();
}
