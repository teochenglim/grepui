// Heatmap, geo map (latitude/longitude on a plain grid, no tiles) and sankey.

import { h } from '../util/dom.js';
import { color, el, fmtUnit, svgRoot } from './svg.js';
import { colKinds } from './shape.js';
import { fmtTime } from '../util/format.js';

export function heatmap(columns, rows, opts = {}) {
  if (!rows.length) return h('div', { class: 'empty' }, 'No data in this range.');
  const [cx, cy, cv] = columns;
  const label = (v) => (typeof v === 'number' && v > 1e11 ? fmtTime(v) : String(v));
  const xs = [...new Set(rows.map((r) => r[cx]))], ys = [...new Set(rows.map((r) => r[cy]))];
  if (typeof xs[0] === 'number') xs.sort((a, b) => a - b);
  const vals = rows.map((r) => r[cv]).filter((v) => v != null);
  const [lo, hi] = [Math.min(...vals), Math.max(...vals)];
  const W = 800, L = 110, B = 22, cellH = Math.min(26, 220 / ys.length), H = ys.length * cellH + B + 6;
  const cw = (W - L - 6) / xs.length;
  const svg = svgRoot(W, H, 'heatmap');
  const at = new Map(rows.map((r) => [`${r[cx]}\u0000${r[cy]}`, r[cv]]));
  ys.forEach((yv, j) => {
    svg.append(el('text', { x: L - 6, y: 6 + j * cellH + cellH / 2 + 4, class: 'tick', 'text-anchor': 'end' }, label(yv).slice(0, 18)));
    xs.forEach((xv, i) => {
      const v = at.get(`${xv}\u0000${yv}`);
      if (v == null) return;
      const rect = el('rect', { x: L + i * cw, y: 6 + j * cellH, width: Math.max(1, cw - 1), height: cellH - 1, fill: 'var(--accent)', opacity: (0.12 + (0.88 * (v - lo)) / (hi - lo || 1)).toFixed(3) });
      rect.append(el('title', {}, `${label(xv)} · ${label(yv)}: ${fmtUnit(v, opts)}`));
      svg.append(rect);
    });
  });
  svg.append(el('text', { x: L, y: H - 6, class: 'tick' }, label(xs[0])), el('text', { x: W - 6, y: H - 6, class: 'tick', 'text-anchor': 'end' }, label(xs[xs.length - 1])));
  return h('div', null, svg, h('p', { class: 'muted' }, `${fmtUnit(lo, opts)} (light) to ${fmtUnit(hi, opts)} (dark)`));
}

export function geoMap(columns, rows, opts = {}) {
  const kinds = colKinds(columns, rows);
  const pts = rows.filter((r) => Math.abs(r[columns[0]]) <= 90 && Math.abs(r[columns[1]]) <= 180);
  if (!pts.length) return h('div', { class: 'empty' }, 'No points with latitude and longitude.');
  const vi = kinds.findIndex((k, i) => i > 1 && k === 'N');
  const li = kinds.findIndex((k, i) => i > 1 && k === 'T');
  const vals = vi >= 0 ? pts.map((r) => r[columns[vi]]) : [];
  const vmax = Math.max(1e-9, ...vals);
  const W = 800, H = 400;
  const svg = svgRoot(W, H, 'map of points', 'chart geo');
  for (let lon = -180; lon <= 180; lon += 30) svg.append(el('line', { x1: ((lon + 180) / 360) * W, x2: ((lon + 180) / 360) * W, y1: 0, y2: H, class: 'grid' }));
  for (let lat = -90; lat <= 90; lat += 30) svg.append(el('line', { x1: 0, x2: W, y1: ((90 - lat) / 180) * H, y2: ((90 - lat) / 180) * H, class: 'grid' }), el('text', { x: 4, y: ((90 - lat) / 180) * H - 3, class: 'tick' }, `${lat}°`));
  pts.forEach((r, i) => {
    const c = el('circle', { cx: ((r[columns[1]] + 180) / 360) * W, cy: ((90 - r[columns[0]]) / 180) * H, r: vi >= 0 ? 3 + 9 * Math.sqrt(r[columns[vi]] / vmax) : 4, fill: color(0), opacity: 0.6 });
    c.append(el('title', {}, `${li >= 0 ? `${r[columns[li]]} · ` : ''}${r[columns[0]]}, ${r[columns[1]]}${vi >= 0 ? `: ${fmtUnit(r[columns[vi]], opts)}` : ''}`));
    svg.append(c);
    void i;
  });
  return h('div', null, svg, h('p', { class: 'muted' }, 'Points on a plain latitude/longitude grid; there is no base map because grepui loads no outside tiles.'));
}

/** Layered sankey: node columns by longest path, link widths by value. Cycles are cut. */
export function sankey(columns, rows, opts = {}) {
  const [cs, ct, cv] = columns;
  const links = rows.filter((r) => r[cs] !== r[ct] && r[cv] > 0).slice(0, 200);
  if (!links.length) return h('div', { class: 'empty' }, 'No flows.');
  const names = [...new Set(links.flatMap((r) => [String(r[cs]), String(r[ct])]))];
  const layer = new Map(names.map((n) => [n, 0]));
  for (let pass = 0; pass < names.length; pass++) {
    let changed = false;
    for (const r of links) {
      const a = String(r[cs]), b = String(r[ct]);
      if (layer.get(b) <= layer.get(a) && layer.get(a) + 1 < names.length) (layer.set(b, layer.get(a) + 1), (changed = true));
    }
    if (!changed) break;
  }
  const cols = Math.max(...layer.values()) + 1;
  const W = 800, padX = 110, nodeW = 14;
  const flow = new Map(names.map((n) => [n, Math.max(links.filter((r) => String(r[cs]) === n).reduce((a, r) => a + r[cv], 0), links.filter((r) => String(r[ct]) === n).reduce((a, r) => a + r[cv], 0))]));
  const byLayer = Array.from({ length: cols }, (_, l) => names.filter((n) => layer.get(n) === l));
  const maxSum = Math.max(...byLayer.map((ns) => ns.reduce((a, n) => a + flow.get(n), 0)));
  const H = Math.max(200, Math.min(520, byLayer.reduce((m, ns) => Math.max(m, ns.length), 0) * 34 + 40));
  const scale = (H - 30) / Math.max(1e-9, maxSum * (1 + 0.06 * Math.max(...byLayer.map((ns) => ns.length))));
  const pos = new Map();
  byLayer.forEach((ns, l) => {
    let y = 10;
    ns.forEach((n) => {
      const hgt = Math.max(3, flow.get(n) * scale);
      pos.set(n, { x: padX + (l * (W - 2 * padX - nodeW)) / Math.max(1, cols - 1), y, h: hgt, outY: 0, inY: 0 });
      y += hgt + 8;
    });
  });
  const svg = svgRoot(W, H, 'sankey diagram', 'chart sankey');
  links.forEach((r) => {
    const a = pos.get(String(r[cs])), b = pos.get(String(r[ct]));
    const w = Math.max(1, r[cv] * scale);
    const y1 = a.y + a.outY + w / 2, y2 = b.y + b.inY + w / 2;
    a.outY += w;
    b.inY += w;
    const x1 = a.x + nodeW, x2 = b.x, mx = (x1 + x2) / 2;
    const p = el('path', { d: `M${x1},${y1} C${mx},${y1} ${mx},${y2} ${x2},${y2}`, class: 'sk-link', 'stroke-width': w.toFixed(1), stroke: color(names.indexOf(String(r[cs]))) });
    p.append(el('title', {}, `${r[cs]} → ${r[ct]}: ${fmtUnit(r[cv], opts)}`));
    svg.append(p);
  });
  names.forEach((n, i) => {
    const p = pos.get(n);
    const right = layer.get(n) === cols - 1 && cols > 1;
    svg.append(el('rect', { x: p.x, y: p.y, width: nodeW, height: p.h, fill: color(i), rx: 2 }), el('text', { x: right ? p.x - 4 : p.x + nodeW + 4, y: p.y + Math.min(p.h, 18) / 2 + 4, class: 'tick', 'text-anchor': right ? 'end' : 'start' }, n.slice(0, 18)));
  });
  return svg;
}
