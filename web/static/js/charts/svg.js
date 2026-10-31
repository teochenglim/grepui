// Small SVG helpers and the colour palette shared by the dashboard charts.

import { fmtVal } from '../components/seriesplot.js';

export const NS = 'http://www.w3.org/2000/svg';
export const PALETTE = ['#0f9d8a', '#4f7cd6', '#d2691e', '#b44fd6', '#c9a227', '#d6455d', '#3aa655', '#7a8699', '#1aa3c4', '#9b6b3a'];
export const color = (i) => PALETTE[i % PALETTE.length];

export function el(tag, attrs = {}, text) {
  const e = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  if (text != null) e.textContent = text;
  return e;
}

export const svgRoot = (w, h, label, cls = 'chart') => el('svg', { viewBox: `0 0 ${w} ${h}`, class: cls, role: 'img', 'aria-label': label, preserveAspectRatio: 'xMidYMid meet' });

/** Format a number with the panel's unit and decimals. */
export function fmtUnit(v, opts = {}) {
  if (v == null || Number.isNaN(Number(v))) return '–';
  const n = Number(v);
  const s = opts.decimals != null ? n.toFixed(opts.decimals) : fmtVal(n);
  return opts.unit ? `${s} ${opts.unit}` : s;
}

/** Nice axis ticks between lo and hi. */
export function ticks(lo, hi, n = 4) {
  if (hi <= lo) return [lo];
  const step = 10 ** Math.floor(Math.log10((hi - lo) / n));
  const mult = [1, 2, 5, 10].find((m) => (hi - lo) / (step * m) <= n) || 10;
  const s = step * mult;
  const out = [];
  for (let t = Math.ceil(lo / s) * s; t <= hi + 1e-9; t += s) out.push(Number(t.toFixed(10)));
  return out;
}

export const legend = (names, h) => {
  const wrap = document.createElement('div');
  wrap.className = 'legend';
  names.slice(0, 12).forEach((n, i) => {
    const s = document.createElement('span');
    s.className = 'lg';
    const dot = document.createElement('span');
    dot.className = 'dot';
    dot.style.background = color(i);
    s.append(dot, n);
    wrap.append(s);
  });
  void h;
  return wrap;
};
