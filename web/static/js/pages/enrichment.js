// Enrichment tables (R-N3): upload a CSV, then join it with logs or traces in SQL mode.

import * as api from '../api.js';
import { dataTable, stateBox } from '../components/table.js';
import { copyText, h, mount } from '../util/dom.js';
import { fmtNum } from '../util/format.js';

export async function enrichmentPage(view) {
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  const msg = h('div');
  const say = (t, kind = '') => mount(msg, t ? h('div', { class: `callout ${kind}` }, t) : null);
  mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Enrichment'), h('span', { class: 'sub' }, 'CSV lookup tables to join with logs and traces')), msg, out);
  const f = {
    name: h('input', { placeholder: 'name (a-z, 0-9, _)', 'aria-label': 'Table name' }),
    key: h('input', { placeholder: 'key column(s), optional', 'aria-label': 'Key columns' }),
    replace: h('input', { type: 'checkbox', 'aria-label': 'Replace if it exists' }),
    file: h('input', { type: 'file', accept: '.csv,text/csv', 'aria-label': 'CSV file' }),
    csv: h('textarea', { rows: 6, spellcheck: 'false', placeholder: 'service,tier,owner\ncart,gold,ann\npayment,gold,bob', 'aria-label': 'CSV text' }),
  };
  f.file.onchange = async () => { if (f.file.files[0]) f.csv.value = await f.file.files[0].text(); };
  const detail = h('div');

  async function load() {
    try {
      const { data } = await api.get('enrichment');
      mount(out,
        data.admin_writes ? null : h('div', { class: 'callout warn' }, 'Uploading and dropping tables needs admin writes: start grepui with --admin-writes.'),
        dataTable({
          columns: [
            { key: 'name', label: 'Table', render: (t) => h('a', { href: '#', onclick: (e) => { e.preventDefault(); show(t.name); } }, t.table) },
            { key: 'rows', label: 'Rows', num: true, render: (t) => fmtNum(t.rows) },
            { key: 'x', label: '', render: (t) => h('button', { type: 'button', class: 'ghost', disabled: !data.admin_writes, onclick: () => confirm(`Drop ${t.table}?`) && api.send('DELETE', `enrichment/${t.name}`).then(() => { mount(detail); return load(); }).catch((e) => say(e.message, 'warn')) }, 'Drop') },
          ],
          rows: data.tables,
          empty: 'No enrichment tables yet.',
        }),
        detail,
        h('div', { class: 'card stack' }, h('h2', null, 'Upload a CSV'),
          h('p', { class: 'muted' }, 'The first row names the columns (letters, digits, _). Types are inferred from every row; at most 5 000 rows, 30 columns, 1 MB. With key columns, a later row replaces an earlier one with the same key. The table is created as enrich_<name>.'),
          h('div', { class: 'row' }, f.name, f.key, h('label', { class: 'row' }, f.replace, 'replace if it exists'), f.file), f.csv,
          h('div', { class: 'row' }, h('button', { class: 'primary', type: 'button', disabled: !data.admin_writes, onclick: upload }, 'Upload'))));
    } catch (e) {
      mount(out, stateBox(e.message, 'error'));
    }
  }

  async function upload() {
    try {
      const org = encodeURIComponent(location.hash.split('/')[1] || 'default');
      const qs = new URLSearchParams({ ...(f.key.value ? { key: f.key.value } : {}), ...(f.replace.checked ? { replace: 'true' } : {}) });
      const res = await fetch(`/api/v1/orgs/${org}/enrichment/${encodeURIComponent(f.name.value)}?${qs}`, { method: 'POST', body: f.csv.value, headers: { Accept: 'application/json', 'Content-Type': 'text/csv' } });
      const b = await res.json();
      if (!res.ok) throw new Error(b.error);
      say(`Created ${b.data.table} with ${b.data.rows} rows: ${b.data.columns.map((c) => `${c.name} ${c.type}`).join(', ')}.`, 'ok');
      await load();
    } catch (e) { say(e.message, 'warn'); }
  }

  async function show(name) {
    const { data } = await api.get(`enrichment/${name}`);
    const cols = data.columns.map((c) => c.column_name).filter((c) => c !== 'ts');
    const stream = h('select', { 'aria-label': 'Stream' }, h('option', { value: 'traces' }, 'traces'), h('option', { value: 'logs' }, 'logs'));
    const left = h('input', { value: 'service', 'aria-label': 'Left side', title: 'service, column:<name> or attr:<key> (logs)' });
    const key = h('select', { 'aria-label': 'Key column' }, cols.map((c) => h('option', { value: c }, c)));
    const show = h('input', { value: cols.filter((c) => c !== cols[0]).slice(0, 2).join(',') || cols[0], 'aria-label': 'Columns to show' });
    const sql = h('pre', { class: 'sqlbox' });
    mount(detail, h('div', { class: 'card stack' }, h('h2', null, `enrich_${name}`),
      dataTable({ columns: cols.map((c) => ({ key: c })), rows: data.rows }),
      h('h2', null, 'Join helper'),
      h('div', { class: 'row' }, 'Join', stream, 'on', left, '=', key, 'showing', show,
        h('button', { type: 'button', onclick: async () => { try { const r = await api.get(`enrichment/${name}/join`, { stream: stream.value, left: left.value, key: key.value, columns: show.value }); mount(sql, r.data.sql); } catch (e) { say(e.message, 'warn'); } } }, 'Build the query'),
        h('button', { type: 'button', onclick: async () => say((await copyText(sql.textContent)) ? 'Copied. Paste it into SQL mode on the Logs page.' : 'Copy failed.', 'ok') }, 'Copy')), sql));
  }
  load();
}
