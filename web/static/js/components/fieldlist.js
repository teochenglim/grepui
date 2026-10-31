// Fields sidebar: columns with types, quick mode, and top values to click into a filter.

import * as api from '../api.js';
import { filterKey } from '../util/logs.js';
import { h, mount } from '../util/dom.js';
import { fmtNum } from '../util/format.js';

const TOP_OK = new Set(['severity_text', 'scope_name', 'scope_version']);

export function fieldList({ fields, params, onPick }) {
  let quick = true;
  const list = h('div', { class: 'fields' });
  const open = new Map(); // field -> container showing its values

  async function showValues(field, box) {
    mount(box, h('span', { class: 'muted' }, 'Loading…'));
    try {
      const { data } = await api.get(`logs/fields/${encodeURIComponent(field)}/values`, { ...params(), limit: 8 });
      mount(box, data.length ? data.map((v) => h('button', { class: 'chip', type: 'button', title: `Filter ${filterKey(field)}=${v.value}`, onclick: () => onPick(field, v.value) }, h('span', null, String(v.value ?? '(none)')), h('span', { class: 'muted' }, fmtNum(v.count)))) : h('span', { class: 'muted' }, 'No values in this range'));
    } catch (e) {
      mount(box, h('span', { class: 'err' }, e.message));
    }
  }

  function row(f) {
    const box = h('div', { class: 'values' });
    const canTop = TOP_OK.has(f.name);
    const head = h('div', { class: 'field' }, h('span', { class: 'fname' }, f.name), h('span', { class: `badge ${f.role}` }, f.type));
    if (canTop) {
      head.classList.add('clickable');
      head.addEventListener('click', () => (box.dataset.open ? (delete box.dataset.open, mount(box)) : ((box.dataset.open = '1'), showValues(f.name, box))));
    }
    if (f.attributes) {
      // JSON attribute columns: ask for one key at a time.
      const key = h('input', { type: 'text', placeholder: 'attribute key, Enter', 'aria-label': `${f.name} key`, spellcheck: 'false' });
      key.addEventListener('keydown', (e) => e.key === 'Enter' && key.value.trim() && showValues(`${f.name}.${key.value.trim()}`, box));
      return h('div', { class: 'field-wrap' }, head, key, box);
    }
    return h('div', { class: 'field-wrap' }, head, box);
  }

  const toggle = h('div', { class: 'seg', role: 'group', 'aria-label': 'Field set' }, ...['Quick', 'All'].map((l) => h('button', { type: 'button', 'aria-pressed': String((l === 'Quick') === quick), onclick: () => { quick = l === 'Quick'; paint(); } }, l)));
  function paint() {
    [...toggle.children].forEach((b) => b.setAttribute('aria-pressed', String((b.textContent === 'Quick') === quick)));
    const shown = fields.filter((f) => !quick || f.quick || f.attributes);
    mount(list, shown.map(row));
  }
  paint();
  return h('aside', { class: 'card fieldbox' }, h('div', { class: 'row between' }, h('h2', null, 'Fields'), toggle), list);
}
