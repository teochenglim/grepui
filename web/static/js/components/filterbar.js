// Search bar: filter text, level, Run. Enter runs; changing the level runs.

import { LEVELS } from '../util/logs.js';
import { h } from '../util/dom.js';

const HELP = [
  ['level=error', 'severity (also the Level menu)'],
  ['http.method=GET', 'attribute equals'],
  ['http.method!=GET', 'attribute differs'],
  ['resource.service.name~pay', 'resource attribute contains'],
  ['"upstream timeout"', 'words in the message (fulltext)'],
  ['trace=<id>   span=<id>   scope=<name>', 'built-in columns'],
];

export function filterBar({ q, level, onSubmit }) {
  const input = h('input', { type: 'search', class: 'grow', placeholder: 'Search logs: level=error http.method=GET "upstream timeout"', 'aria-label': 'Filter logs', value: q, spellcheck: 'false', autocomplete: 'off' });
  const lvl = h('select', { 'aria-label': 'Level' }, LEVELS.map((l) => h('option', { value: l, selected: l === level }, l || 'Any level')));
  const run = h('button', { class: 'primary', type: 'button' }, 'Run');
  const help = h('details', { class: 'help' }, h('summary', null, 'Syntax'), h('table', null, h('tbody', null, HELP.map(([c, d]) => h('tr', null, h('td', null, h('code', null, c)), h('td', { class: 'muted' }, d))))));
  const submit = () => onSubmit({ q: input.value.trim(), level: lvl.value });
  input.addEventListener('keydown', (e) => e.key === 'Enter' && submit());
  lvl.addEventListener('change', submit);
  run.addEventListener('click', submit);
  return {
    el: h('div', { class: 'filterbar' }, h('div', { class: 'row' }, input, lvl, run), help),
    get: () => ({ q: input.value.trim(), level: lvl.value }),
    set: ({ q: nq, level: nl }) => {
      if (nq != null) input.value = nq;
      if (nl != null) lvl.value = nl;
    },
  };
}
