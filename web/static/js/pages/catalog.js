// Service catalog (R-V4): every service with first and last seen, RED summary and its note.

import * as api from '../api.js';
import { annotationForm, noteView } from '../components/annotation.js';
import { dataTable, stateBox } from '../components/table.js';
import { timeWindow } from '../state.js';
import { h, mount } from '../util/dom.js';
import { fmtAge, fmtNum, fmtUs, serviceColor } from '../util/format.js';
import { errBadge, serviceHref } from './services.js';

export async function catalogPage(view) {
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  const editor = h('div');
  mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Catalog'), h('span', { class: 'sub' }, 'services with owners, tiers and runbooks')), editor, out);
  const w = timeWindow();
  async function load() {
    try {
      const { data } = await api.get('catalog', { from: w.from, to: w.to });
      mount(out, dataTable({
        rows: data, empty: 'No services yet. See Connect to send traces.',
        columns: [
          { key: 'service_name', label: 'Service', render: (r) => h('span', null, h('span', { class: 'dot', style: `background:${serviceColor(r.service_name)}` }), r.seen ? h('a', { href: serviceHref(r.service_name) }, r.service_name) : r.service_name) },
          { key: 'annotation', label: 'Note', render: (r) => noteView(r.annotation) || h('span', { class: 'muted' }, '–') },
          { key: 'first_seen_ms', label: 'First seen', render: (r) => (r.seen ? fmtAge(Number(r.first_seen_ms)) : '–') },
          { key: 'last_seen_ms', label: 'Last seen', render: (r) => (r.seen ? fmtAge(Number(r.last_seen_ms)) : 'not in this range') },
          { key: 'requests', label: 'Requests', num: true, render: (r) => (r.seen ? fmtNum(r.requests) : '') },
          { key: 'error_pct', label: 'Errors', num: true, render: (r) => (r.seen ? errBadge(r.error_pct) : '') },
          { key: 'p95_us', label: 'p95', num: true, render: (r) => (r.seen ? fmtUs(r.p95_us) : '') },
          { key: 'edit', label: '', render: (r) => h('button', { type: 'button', onclick: () => edit(r) }, 'Edit') },
        ],
      }));
    } catch (e) {
      mount(out, stateBox(e.message, 'error'));
    }
  }
  function edit(r) {
    mount(editor, h('div', { class: 'card' }, h('h2', null, `Note for ${r.service_name}`), annotationForm(r.service_name, r.annotation, () => (mount(editor), load())), h('button', { type: 'button', class: 'ghost', onclick: () => mount(editor) }, 'Close')));
    editor.scrollIntoView({ block: 'nearest' });
  }
  load();
}
