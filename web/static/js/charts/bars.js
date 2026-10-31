// Horizontal bars and stacked horizontal bars: one row per category.

import { h } from '../util/dom.js';
import { color, fmtUnit, legend } from './svg.js';
import { toSeries } from './shape.js';

export function hbars(type, columns, rows, opts = {}) {
  const { series, categorical } = toSeries(columns, rows);
  const cats = categorical ? [...new Set(series.flatMap((s) => s.points.map((p) => p[0])))] : [...new Set(series.flatMap((s) => s.points.map((p) => String(p[0]))))];
  if (!cats.length) return h('div', { class: 'empty' }, 'No data in this range.');
  const value = (s, c) => s.points.find((p) => String(p[0]) === String(c))?.[1] ?? 0;
  const stacked = type === 'stacked_bar_h';
  const totals = cats.map((c) => series.reduce((a, s) => a + value(s, c), 0));
  const max = Math.max(1e-9, ...(stacked ? totals : cats.flatMap((c) => series.map((s) => value(s, c)))));
  return h('div', null,
    h('div', { class: 'bars' }, cats.slice(0, 40).map((c, ci) => h('div', { class: 'barrow' },
      h('span', { class: 'barname clip', title: c }, c),
      h('span', { class: `bartrack${stacked ? '' : ' multi'}` }, series.map((s, i) => h('span', { class: 'barfill', title: `${s.name}: ${fmtUnit(value(s, c), opts)}`, style: `width:${Math.max(0, (value(s, c) / max) * 100)}%;background:${color(i)}` }))),
      h('span', { class: 'barval' }, fmtUnit(stacked ? totals[ci] : value(series[0], c), opts))))),
    series.length > 1 ? legend(series.map((s) => s.name)) : null);
}
