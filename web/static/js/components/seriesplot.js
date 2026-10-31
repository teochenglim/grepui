// Many series on one time axis, with a legend; plus a bar list of the latest values.

import { fmtTime } from '../util/format.js';
import { h } from '../util/dom.js';

const NS = 'http://www.w3.org/2000/svg';
const el = (tag, attrs = {}, text) => {
  const e = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  if (text != null) e.textContent = text;
  return e;
};
const PALETTE = ['#0f9d8a', '#4f7cd6', '#d2691e', '#b44fd6', '#c9a227', '#d6455d', '#3aa655', '#7a8699'];

export const seriesName = (s) => {
  const e = Object.entries(s.labels || {});
  return e.length ? e.map(([k, v]) => `${k}="${v}"`).join(', ') : '(no labels)';
};

/** Compact number: 1.2k, 3.4M, 0.25. */
export function fmtVal(v) {
  const a = Math.abs(v);
  if (a >= 1e9) return `${(v / 1e9).toFixed(2)}G`;
  if (a >= 1e6) return `${(v / 1e6).toFixed(2)}M`;
  if (a >= 1e4) return `${(v / 1e3).toFixed(1)}k`;
  if (a >= 100) return v.toFixed(0);
  if (a >= 1) return v.toFixed(2);
  return a === 0 ? '0' : v.toPrecision(2);
}

const MAX_LEGEND = 12;

/** series: [{ labels, points: [[t, v]] }]. */
export function seriesPlot(series, from, to) {
  const W = 1000, H = 240, L = 60, B = 20, T = 10;
  const all = series.flatMap((s) => s.points);
  const max = Math.max(1e-9, ...all.map((p) => p[1]));
  const min = Math.min(0, ...all.map((p) => p[1]));
  const x = (t) => L + ((t - from) / Math.max(1, to - from)) * (W - L - 8);
  const y = (v) => H - B - ((v - min) / Math.max(1e-9, max - min)) * (H - B - T);
  const svg = el('svg', { viewBox: `0 0 ${W} ${H}`, class: 'line series', role: 'img', 'aria-label': `${series.length} series over time` });
  for (const v of [min, (min + max) / 2, max]) svg.append(el('line', { x1: L, x2: W - 8, y1: y(v), y2: y(v), class: 'grid' }), el('text', { x: L - 6, y: y(v) + 4, class: 'tick', 'text-anchor': 'end' }, fmtVal(v)));
  series.forEach((s, i) => {
    const color = PALETTE[i % PALETTE.length];
    const d = s.points.map(([t, v], j) => `${j ? 'L' : 'M'}${x(t).toFixed(1)},${y(v).toFixed(1)}`).join(' ');
    const p = el('path', { d, class: 'path', style: `stroke:${color}` });
    p.append(el('title', {}, seriesName(s)));
    svg.append(p);
    if (s.points.length === 1) svg.append(el('circle', { cx: x(s.points[0][0]), cy: y(s.points[0][1]), r: 3, style: `fill:${color}` }));
  });
  const legend = h('div', { class: 'legend' }, series.slice(0, MAX_LEGEND).map((s, i) => h('span', { class: 'lg' }, h('span', { class: 'dot', style: `background:${PALETTE[i % PALETTE.length]}` }), seriesName(s))),
    series.length > MAX_LEGEND ? h('span', { class: 'muted' }, `and ${series.length - MAX_LEGEND} more`) : null);
  const axis = h('div', { class: 'hist-axis muted' }, h('span', null, fmtTime(from)), h('span', null, fmtTime(to)));
  return h('div', null, svg, axis, legend);
}

/** The newest value of each series as horizontal bars. */
export function barList(series) {
  const rows = series.map((s) => ({ s, v: s.points.length ? s.points[s.points.length - 1][1] : 0 })).sort((a, b) => b.v - a.v).slice(0, 30);
  const max = Math.max(1e-9, ...rows.map((r) => Math.abs(r.v)));
  return h('div', { class: 'bars' }, rows.map((r, i) => h('div', { class: 'barrow' }, h('span', { class: 'barname clip', title: seriesName(r.s) }, seriesName(r.s)), h('span', { class: 'bartrack' }, h('span', { class: 'barfill', style: `width:${Math.max(1, (Math.abs(r.v) / max) * 100)}%;background:${PALETTE[i % PALETTE.length]}` })), h('span', { class: 'barval' }, fmtVal(r.v)))));
}
