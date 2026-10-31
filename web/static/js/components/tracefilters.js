// Trace search bar: filter text, service, minimum duration, errors only.

import * as api from '../api.js';
import { timeWindow } from '../state.js';
import { h } from '../util/dom.js';

const HELP = [
  ['http.route=GET', 'span attribute equals (any span_attributes.* column)'],
  ['resource.service.version~1.4', 'resource attribute contains'],
  ['status=error   kind=server', 'status and kind shorthands'],
  ['operation~Charge   Charge', 'span name contains'],
  ['trace=<id>   span=<id>', 'ids'],
];

export function traceFilters({ q, service, minMs, errors, onSubmit }) {
  const input = h('input', { type: 'search', class: 'grow', placeholder: 'Search spans: http.route=GET status=error Charge', 'aria-label': 'Filter traces', value: q, spellcheck: 'false', autocomplete: 'off' });
  const svc = h('select', { 'aria-label': 'Service' }, h('option', { value: '' }, 'Any service'));
  const dur = h('input', { type: 'number', min: '0', step: '1', placeholder: 'min ms', 'aria-label': 'Minimum duration in milliseconds', value: minMs || '', style: 'width: 96px' });
  const err = h('input', { type: 'checkbox', checked: errors, 'aria-label': 'Errors only' });
  const run = h('button', { class: 'primary', type: 'button' }, 'Run');
  const get = () => ({ q: input.value.trim(), service: svc.value, minMs: dur.value.trim(), errors: err.checked });
  const submit = () => onSubmit(get());
  input.addEventListener('keydown', (e) => e.key === 'Enter' && submit());
  for (const el of [svc, err]) el.addEventListener('change', submit);
  dur.addEventListener('keydown', (e) => e.key === 'Enter' && submit());
  run.addEventListener('click', submit);
  // Services seen in the selected range.
  const w = timeWindow();
  api.get('traces/services', { from: w.from, to: w.to }).then(({ data }) => {
    for (const n of data) svc.append(h('option', { value: n, selected: n === service }, n));
    if (service && !data.includes(service)) svc.append(h('option', { value: service, selected: true }, service));
  }).catch(() => {});
  const help = h('details', { class: 'help' }, h('summary', null, 'Syntax'), h('table', null, h('tbody', null, HELP.map(([c, d]) => h('tr', null, h('td', null, h('code', null, c)), h('td', { class: 'muted' }, d))))));
  return { el: h('div', { class: 'filterbar' }, h('div', { class: 'row' }, input, svc, dur, h('label', { class: 'check' }, err, 'Errors only'), run), help), get };
}
