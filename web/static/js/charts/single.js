// Metric text (stat) and gauge: one number.

import { h } from '../util/dom.js';
import { el, fmtUnit, svgRoot } from './svg.js';
import { colKinds } from './shape.js';

function firstNumber(columns, rows) {
  const kinds = colKinds(columns, rows);
  const i = kinds.indexOf('N');
  return i < 0 || !rows.length ? null : { v: rows[0][columns[i]], name: columns[i] };
}

export function stat(columns, rows, opts = {}) {
  const f = firstNumber(columns, rows);
  if (!f) return h('div', { class: 'empty' }, 'No value.');
  return h('div', { class: 'bigstat' }, h('span', { class: 'bigvalue' }, fmtUnit(f.v, opts)), h('span', { class: 'muted' }, f.name));
}

export function gauge(columns, rows, opts = {}) {
  const f = firstNumber(columns, rows);
  if (!f) return h('div', { class: 'empty' }, 'No value.');
  const min = opts.min ?? 0, max = opts.max ?? 100;
  const frac = Math.max(0, Math.min(1, (f.v - min) / (max - min || 1)));
  const svg = svgRoot(200, 120, 'gauge', 'chart gauge');
  const pt = (a, r) => `${(100 + r * Math.cos(a)).toFixed(2)},${(100 + r * Math.sin(a)).toFixed(2)}`;
  const arc = (a0, a1) => `M${pt(a0, 80)} A80,80 0 ${a1 - a0 > Math.PI ? 1 : 0} 1 ${pt(a1, 80)}`;
  svg.append(el('path', { d: arc(Math.PI, 2 * Math.PI), class: 'gauge-bg' }), el('path', { d: arc(Math.PI, Math.PI + frac * Math.PI), class: `gauge-fg${frac > 0.9 ? ' hot' : ''}` }), el('text', { x: 100, y: 96, 'text-anchor': 'middle', class: 'gauge-v' }, fmtUnit(f.v, opts)), el('text', { x: 20, y: 114, class: 'tick' }, String(min)), el('text', { x: 180, y: 114, class: 'tick', 'text-anchor': 'end' }, String(max)));
  return h('div', { class: 'gaugewrap' }, svg);
}
