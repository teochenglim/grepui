// A small line chart over time buckets: one series, missing buckets drawn as gaps.

import { fmtTime } from '../util/format.js';

const NS = 'http://www.w3.org/2000/svg';
const el = (tag, attrs = {}, text) => {
  const e = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  if (text != null) e.textContent = text;
  return e;
};

/** points: [{ t (ms), v }]; step: bucket ms; fmt: value formatter; cls: 'bad' for error charts. */
export function lineChart({ points, from, to, step, fmt, label, cls = '' }) {
  const W = 1000, H = 130, L = 56, B = 18, T = 8;
  const svg = el('svg', { viewBox: `0 0 ${W} ${H}`, class: `line ${cls}`, role: 'img', 'aria-label': label });
  const max = Math.max(1e-9, ...points.map((p) => p.v));
  const x = (t) => L + ((t - from) / Math.max(1, to - from)) * (W - L - 6);
  const y = (v) => H - B - (v / max) * (H - B - T);
  svg.append(el('line', { x1: L, x2: W - 6, y1: y(0), y2: y(0), class: 'axis' }), el('line', { x1: L, x2: W - 6, y1: y(max), y2: y(max), class: 'grid' }), el('text', { x: L - 6, y: y(max) + 4, class: 'tick', 'text-anchor': 'end' }, fmt(max)), el('text', { x: L - 6, y: y(0) + 4, class: 'tick', 'text-anchor': 'end' }, fmt(0)));
  const sorted = [...points].sort((a, b) => a.t - b.t);
  let d = '';
  let prev = null;
  for (const p of sorted) {
    d += `${prev !== null && p.t - prev <= step * 1.5 ? 'L' : 'M'}${x(p.t + step / 2).toFixed(1)},${y(p.v).toFixed(1)} `;
    prev = p.t;
  }
  svg.append(el('path', { d, class: 'path' }));
  for (const p of sorted) {
    const dot = el('circle', { cx: x(p.t + step / 2), cy: y(p.v), r: 2.6, class: 'dot' });
    dot.append(el('title', {}, `${fmtTime(p.t)}: ${fmt(p.v)}`));
    svg.append(dot);
  }
  const axis = document.createElement('div');
  axis.className = 'hist-axis muted';
  axis.append(...[fmtTime(from), fmtTime(to)].map((t) => Object.assign(document.createElement('span'), { textContent: t })));
  const wrap = document.createElement('div');
  wrap.append(svg, axis);
  return wrap;
}
