// Line, area, stacked area, scatter, vertical bars and stacked vertical bars.

import { fmtTime } from '../util/format.js';
import { color, el, fmtUnit, legend, svgRoot, ticks } from './svg.js';
import { toSeries } from './shape.js';
import { h } from '../util/dom.js';

const W = 800, H = 260, L = 84, R = 10, T = 10, B = 28;
const isTime = (x) => x > 1e11;

export function xyChart(type, columns, rows, opts = {}, compare = null) {
  const main = toSeries(columns, rows);
  const past = compare ? toSeries(columns, compare.rows).series.map((s) => ({ ...s, name: `${s.name} (past)`, past: true })) : [];
  const all = [...main.series, ...past];
  if (!all.some((s) => s.points.length)) return h('div', { class: 'empty' }, 'No data in this range.');
  const stacked = type === 'stacked_area' || type === 'stacked_bar_v';
  const bars = type === 'bar_v' || type === 'stacked_bar_v';
  const cats = main.categorical ? [...new Set(main.series.flatMap((s) => s.points.map((p) => p[0])))] : null;
  const xs = cats ? cats.map((_, i) => i) : main.series.flatMap((s) => s.points.map((p) => p[0])).concat(past.flatMap((s) => s.points.map((p) => p[0])));
  const xKey = (p) => (cats ? cats.indexOf(p[0]) : p[0]);
  let xlo = Math.min(...xs), xhi = Math.max(...xs);
  if (cats) [xlo, xhi] = [-0.5, cats.length - 0.5];
  else if (bars) {
    const sorted = [...new Set(xs)].sort((a, b) => a - b);
    const gap = sorted.length > 1 ? Math.min(...sorted.slice(1).map((v, i) => v - sorted[i])) : 1;
    [xlo, xhi] = [xlo - gap / 2, xhi + gap / 2];
  }
  if (xhi === xlo) xhi = xlo + 1;
  // Stacking: cumulative y per x across the main series.
  const base = new Map();
  const layers = main.series.map((s) => s.points.map((p) => {
    const k = xKey(p);
    const y0 = stacked ? base.get(k) || 0 : 0;
    if (stacked) base.set(k, y0 + p[1]);
    return { k, y0, y1: y0 + p[1], v: p[1] };
  }));
  const ys = layers.flat().flatMap((p) => [p.y1, p.y0]).concat(past.flatMap((s) => s.points.map((p) => p[1])));
  let ylo = Math.min(0, ...ys), yhi = Math.max(...ys);
  if (yhi === ylo) yhi = ylo + 1;
  const x = (k) => L + ((k - xlo) / (xhi - xlo)) * (W - L - R);
  const y = (v) => H - B - ((v - ylo) / (yhi - ylo)) * (H - B - T);
  const svg = svgRoot(W, H, `${type} chart`);
  for (const t of ticks(ylo, yhi)) svg.append(el('line', { x1: L, x2: W - R, y1: y(t), y2: y(t), class: 'grid' }), el('text', { x: L - 6, y: y(t) + 4, class: 'tick', 'text-anchor': 'end' }, fmtUnit(t, { unit: opts.unit })));
  const label = (k) => (cats ? cats[Math.round(k)] : isTime(k) ? fmtTime(k) : String(k));
  const nx = Math.min(6, cats ? cats.length : 5);
  for (let i = 0; i < nx; i++) {
    const k = cats ? Math.round((i / Math.max(1, nx - 1)) * (cats.length - 1)) : xlo + ((xhi - xlo) * i) / Math.max(1, nx - 1);
    const t = el('text', { x: x(k), y: H - 8, class: 'tick', 'text-anchor': i === 0 ? 'start' : i === nx - 1 ? 'end' : 'middle' }, String(label(k)).slice(0, 19));
    svg.append(t);
  }
  svg.append(el('line', { x1: L, x2: W - R, y1: y(0), y2: y(0), class: 'axis' }));
  const tip = (node, text) => (node.append(el('title', {}, text)), node);
  main.series.forEach((s, i) => {
    const c = color(i);
    const pts = layers[i];
    if (bars) {
      const n = stacked ? 1 : main.series.length;
      const slot = ((W - L - R) / (xhi - xlo)) * (cats ? 1 : Math.min(...[...new Set(xs)].sort((a, b) => a - b).slice(1).map((v, j, a2) => v - (j ? a2[j - 1] : [...new Set(xs)].sort((p, q) => p - q)[0])).concat([xhi - xlo]))) ;
      const bw = Math.max(1, Math.min(60, slot * 0.8) / n);
      pts.forEach((p) => svg.append(tip(el('rect', { x: x(p.k) - (bw * n) / 2 + (stacked ? 0 : i * bw), y: y(Math.max(p.y0, p.y1)), width: bw, height: Math.max(1, Math.abs(y(p.y0) - y(p.y1))), fill: c, opacity: 0.85 }), `${s.name} · ${label(p.k)}: ${fmtUnit(p.v, opts)}`)));
    } else if (type === 'scatter') {
      pts.forEach((p) => svg.append(tip(el('circle', { cx: x(p.k), cy: y(p.y1), r: 3, fill: c, opacity: 0.75 }), `${s.name} · ${label(p.k)}: ${fmtUnit(p.v, opts)}`)));
    } else {
      const top = pts.map((p, j) => `${j ? 'L' : 'M'}${x(p.k).toFixed(1)},${y(p.y1).toFixed(1)}`).join(' ');
      if (type === 'area' || type === 'stacked_area') {
        const bottom = [...pts].reverse().map((p) => `L${x(p.k).toFixed(1)},${y(p.y0).toFixed(1)}`).join(' ');
        svg.append(el('path', { d: `${top} ${bottom} Z`, fill: c, opacity: 0.3 }));
      }
      svg.append(tip(el('path', { d: top, class: 'path', style: `stroke:${c}` }), s.name));
    }
  });
  past.forEach((s) => {
    const d = s.points.map((p, j) => `${j ? 'L' : 'M'}${x(xKey(p)).toFixed(1)},${y(p[1]).toFixed(1)}`).join(' ');
    svg.append(el('path', { d, class: 'path past', style: `stroke:${color(main.series.findIndex((m) => `${m.name} (past)` === s.name))}` }));
  });
  const names = main.series.map((s) => s.name).concat(past.length ? ['dashed = same period earlier'] : []);
  return h('div', null, svg, opts.legend === false ? null : legend(names));
}
