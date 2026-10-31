// Saved SQL functions (R-N2): snippets with positional parameters, called as @name(1, 'x') in SQL mode and panels.

import * as api from '../api.js';
import { dataTable, stateBox } from '../components/table.js';
import { h, mount } from '../util/dom.js';

export async function functionsPage(view) {
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  const msg = h('div');
  const say = (t, kind = '') => mount(msg, t ? h('div', { class: `callout ${kind}` }, t) : null);
  mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Functions'), h('span', { class: 'sub' }, 'saved SQL snippets for SQL mode and dashboard panels')), msg, out);
  const f = {
    name: h('input', { placeholder: 'name, e.g. slow_spans', 'aria-label': 'Name' }),
    params: h('input', { placeholder: 'parameters, e.g. ms, svc', 'aria-label': 'Parameters' }),
    desc: h('input', { class: 'grow', placeholder: 'description', 'aria-label': 'Description' }),
    body: h('textarea', { rows: 4, spellcheck: 'false', placeholder: "(duration_nano / 1000000 > $1 AND service_name = $2)", 'aria-label': 'Body' }),
  };
  async function load() {
    try {
      const { data } = await api.get('functions');
      const call = (x) => `@${x.name}(${x.params.map((_, i) => (i === 0 ? '250' : "'value'")).join(', ')})`;
      mount(out,
        dataTable({
          columns: [
            { key: 'name', label: 'Function', render: (x) => h('a', { href: '#', onclick: (e) => { e.preventDefault(); f.name.value = x.name; f.params.value = x.params.join(', '); f.desc.value = x.description; f.body.value = x.body; } }, x.name) },
            { key: 'p', label: 'Parameters', render: (x) => x.params.map((p, i) => `$${i + 1} ${p}`).join(', ') || '–' },
            { key: 'body', label: 'Body', render: (x) => h('code', { class: 'muted' }, x.body.slice(0, 80)) },
            { key: 'u', label: 'Call', render: (x) => h('code', null, call(x)) },
            { key: 'x', label: '', render: (x) => h('button', { type: 'button', class: 'ghost', onclick: () => confirm(`Delete "${x.name}"?`) && api.send('DELETE', `functions/${x.name}`).then(load).catch((e) => say(e.message, 'warn')) }, 'Delete') },
          ],
          rows: data,
          empty: 'No functions yet.',
        }),
        h('div', { class: 'card stack' }, h('h2', null, 'Save a function'),
          h('p', { class: 'muted' }, 'The body is an SQL expression with $1, $2, … for the parameters. A call passes literals only (numbers, \'text\', true, false); they are sent as bound parameters. A body cannot call functions. Functions work in SQL mode and dashboard SQL panels, not in alerts.'),
          h('div', { class: 'row' }, f.name, f.params, f.desc), f.body,
          h('div', { class: 'row' }, h('button', { class: 'primary', type: 'button', onclick: async () => {
            try {
              await api.send('PUT', `functions/${f.name.value}`, { params: f.params.value.split(',').map((x) => x.trim()).filter(Boolean), description: f.desc.value, body: f.body.value });
              say('Saved.', 'ok');
              await load();
            } catch (e) { say(e.message, 'warn'); }
          } }, 'Save'))));
    } catch (e) {
      mount(out, stateBox(e.message, 'error'));
    }
  }
  load();
}
