// Latency over time: one point per trace, log scale on duration, errors in red. Click opens it.

import { fmtNum, fmtTime, fmtUs } from '../util/format.js';

const NS = 'http://www.w3.org/2000/svg';
const el = (tag, attrs = {}, text) => {
  const e = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  if (text != null) e.textContent = text;
  return e;
};

export function scatter({ points, capped, max }, from, to, onOpen) {
  const W = 1000, H = 190, L = 46, B = 20, T = 8;
  const svg = el('svg', { viewBox: `0 0 ${W} ${H}`, class: 'scatter', role: 'img', 'aria-label': `${fmtNum(points.length)} traces by start time and duration` });
  const durs = points.map((p) => Math.max(1, Number(p.duration_us)));
  const lo = Math.floor(Math.log10(Math.max(1, Math.min(...durs, 1000)))), hi = Math.max(lo + 1, Math.ceil(Math.log10(Math.max(...durs, 10))));
  const y = (us) => H - B - ((Math.log10(Math.max(1, us)) - lo) / (hi - lo)) * (H - B - T);
  const x = (ms) => L + ((ms - from) / (to - from)) * (W - L - 6);
  for (let e = lo; e <= hi; e++) {
    svg.append(el('line', { x1: L, x2: W - 6, y1: y(10 ** e), y2: y(10 ** e), class: 'grid' }), el('text', { x: L - 6, y: y(10 ** e) + 4, class: 'tick', 'text-anchor': 'end' }, fmtUs(10 ** e)));
  }
  for (const p of points) {
    const c = el('circle', { cx: x(Number(p.start_ms)), cy: y(Number(p.duration_us)), r: 3.2, class: Number(p.errors) > 0 ? 'pt err' : 'pt', tabindex: '0', role: 'link', 'aria-label': `${p.root_service || ''} ${p.root_operation || ''} ${fmtUs(p.duration_us)}` });
    c.append(el('title', {}, `${p.root_service || '?'} · ${p.root_operation || '?'}\n${fmtUs(p.duration_us)} · ${p.spans} spans${Number(p.errors) ? ` · ${p.errors} errors` : ''}\n${fmtTime(p.start_ms)}`));
    c.addEventListener('click', () => onOpen(p));
    c.addEventListener('keydown', (e) => e.key === 'Enter' && onOpen(p));
    svg.append(c);
  }
  const wrap = document.createElement('div');
  wrap.append(svg);
  if (capped) {
    const n = document.createElement('p');
    n.className = 'muted';
    n.textContent = `Showing the newest ${max} traces. Narrow the range or add a filter to see the rest.`;
    wrap.append(n);
  }
  return wrap;
}
