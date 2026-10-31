// The service map: nodes in columns (callers left of callees), edges as arrows whose width follows
// the call volume and whose colour turns red with the error rate. The layout comes from the API.

import { fmtNum, serviceColor } from '../util/format.js';

const NS = 'http://www.w3.org/2000/svg';
const el = (tag, attrs = {}, text) => {
  const e = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  if (text != null) e.textContent = text;
  return e;
};
const NODE_W = 150, NODE_H = 44;

export function serviceMap({ nodes, edges, width, height }, onOpen) {
  const svg = el('svg', { viewBox: `0 0 ${width} ${height}`, class: 'smap', role: 'img', 'aria-label': `${nodes.length} services and ${edges.length} dependencies`, style: `min-width:${Math.min(width, 900)}px` });
  const defs = el('defs');
  for (const [id, cls] of [['arrow', 'ok'], ['arrow-bad', 'bad']]) {
    const m = el('marker', { id, viewBox: '0 0 10 10', refX: 9, refY: 5, markerWidth: 9, markerHeight: 9, markerUnits: 'userSpaceOnUse', orient: 'auto-start-reverse' });
    m.append(el('path', { d: 'M0,0 L10,5 L0,10 z', class: `arrowhead ${cls}` }));
    defs.append(m);
  }
  svg.append(defs);
  const at = new Map(nodes.map((n) => [n.id, n]));
  const maxCalls = Math.max(1, ...edges.map((e) => Number(e.calls)));
  for (const e of edges) {
    const a = at.get(e.from), b = at.get(e.to);
    if (!a || !b) continue;
    const bad = Number(e.error_pct) >= 1;
    const w = 1 + 4 * Math.log10(1 + Number(e.calls)) / Math.log10(1 + maxCalls);
    let d, lx, ly;
    if (e.self) {
      const x = a.x + NODE_W / 2, y = a.y;
      d = `M${x - 20},${y} C${x - 40},${y - 50} ${x + 40},${y - 50} ${x + 20},${y}`;
      lx = x; ly = y - 40;
    } else if (e.back) {
      const x1 = a.x + NODE_W / 2, y1 = a.y + NODE_H, x2 = b.x + NODE_W / 2, y2 = b.y + NODE_H;
      const dip = Math.max(y1, y2) + 36;
      d = `M${x1},${y1} C${x1},${dip} ${x2},${dip} ${x2},${y2}`;
      lx = (x1 + x2) / 2; ly = dip - 6;
    } else {
      const x1 = a.x + NODE_W, y1 = a.y + NODE_H / 2, x2 = b.x, y2 = b.y + NODE_H / 2, mx = (x1 + x2) / 2;
      if (b.layer - a.layer > 1) {
        // Skips a column: arc over the nodes in between instead of passing behind them.
        const lift = Math.min(y1, y2) - 70;
        d = `M${x1},${y1} C${x1 + 80},${lift} ${x2 - 80},${lift} ${x2 - 2},${y2}`;
        lx = mx; ly = (y1 + y2) / 2 - 62;
      } else {
        d = `M${x1},${y1} C${mx},${y1} ${mx},${y2} ${x2 - 2},${y2}`;
        lx = mx; ly = (y1 + y2) / 2 - 5;
      }
    }
    const g = el('g', { class: `edge${bad ? ' bad' : ''}` });
    const path = el('path', { d, class: 'edge-path', 'stroke-width': w.toFixed(1), 'marker-end': `url(#${bad ? 'arrow-bad' : 'arrow'})` });
    path.append(el('title', {}, `${e.from} → ${e.to}\n${fmtNum(e.calls)} calls · ${fmtNum(e.errors)} errors (${Number(e.error_pct).toFixed(1)}%)`));
    g.append(path, el('text', { x: lx, y: ly, class: 'edge-label', 'text-anchor': 'middle' }, fmtNum(e.calls)));
    svg.append(g);
  }
  for (const n of nodes) {
    const g = el('g', { class: 'node', tabindex: '0', role: 'link', 'aria-label': `Open service ${n.id}`, transform: `translate(${n.x},${n.y})` });
    g.append(el('rect', { width: NODE_W, height: NODE_H, rx: 8, class: 'node-box', style: `stroke:${serviceColor(n.id)}` }), el('text', { x: 12, y: 19, class: 'node-name' }, (n.label || n.id).length > 18 ? `${(n.label || n.id).slice(0, 17)}…` : (n.label || n.id)), el('text', { x: 12, y: 35, class: 'node-sub' }, `${fmtNum(n.calls_in)} in · ${fmtNum(n.calls_out)} out`));
    g.append(el('title', {}, n.id));
    g.addEventListener('click', () => onOpen(n.id));
    g.addEventListener('keydown', (ev) => ev.key === 'Enter' && onOpen(n.id));
    svg.append(g);
  }
  return svg;
}
