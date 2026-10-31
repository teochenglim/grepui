// Pie and donut: shares of a whole.

import { h } from '../util/dom.js';
import { color, el, fmtUnit, svgRoot } from './svg.js';
import { toPairs } from './shape.js';

export function pie(type, columns, rows, opts = {}) {
  const pairs = toPairs(columns, rows).filter((p) => p[1] > 0).slice(0, 30);
  const total = pairs.reduce((a, p) => a + p[1], 0);
  if (!total) return h('div', { class: 'empty' }, 'No data in this range.');
  const R = 90, C = 100, inner = type === 'donut' ? 52 : 0;
  const svg = svgRoot(200, 200, `${type} chart`, 'chart pie');
  let a0 = -Math.PI / 2;
  pairs.forEach(([name, v], i) => {
    const a1 = a0 + (v / total) * Math.PI * 2;
    const big = a1 - a0 > Math.PI ? 1 : 0;
    const pt = (r, a) => `${(C + r * Math.cos(a)).toFixed(2)},${(C + r * Math.sin(a)).toFixed(2)}`;
    const full = pairs.length === 1;
    const d = full
      ? `M${C - R},${C} a${R},${R} 0 1,0 ${2 * R},0 a${R},${R} 0 1,0 ${-2 * R},0 ${inner ? `M${C - inner},${C} a${inner},${inner} 0 1,1 ${2 * inner},0 a${inner},${inner} 0 1,1 ${-2 * inner},0` : ''}`
      : `M${pt(R, a0)} A${R},${R} 0 ${big} 1 ${pt(R, a1)} ${inner ? `L${pt(inner, a1)} A${inner},${inner} 0 ${big} 0 ${pt(inner, a0)} Z` : `L${C},${C} Z`}`;
    const p = el('path', { d, fill: color(i), 'fill-rule': 'evenodd', stroke: 'var(--surface)', 'stroke-width': 1 });
    p.append(el('title', {}, `${name}: ${fmtUnit(v, opts)} (${((v / total) * 100).toFixed(1)}%)`));
    svg.append(p);
    a0 = a1;
  });
  return h('div', { class: 'piewrap' }, svg, h('div', { class: 'legend col' }, pairs.map(([name, v], i) => h('span', { class: 'lg' }, h('span', { class: 'dot', style: `background:${color(i)}` }), `${name}\u00a0`, h('span', { class: 'muted' }, `${((v / total) * 100).toFixed(1)}%`)))));
}
