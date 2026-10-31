// Hits over time as SVG bars. Gaps in the data are drawn as empty buckets.

import { fmtNum, fmtTime } from '../util/format.js';

const NS = 'http://www.w3.org/2000/svg';
const el = (tag, attrs = {}, text) => {
  const e = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  if (text != null) e.textContent = text;
  return e;
};

export function histogram({ buckets, bucket_ms: step, total }, from, to, unit = 'lines') {
  const W = 1000, H = 110, padB = 18;
  const counts = new Map(buckets.map((b) => [Number(b.t), Number(b.count)]));
  const start = Math.floor(from / step) * step;
  const slots = [];
  for (let t = start; t < to; t += step) slots.push([t, counts.get(t) ?? 0]);
  const max = Math.max(1, ...slots.map(([, c]) => c));
  const bw = W / Math.max(1, slots.length);
  const svg = el('svg', { viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: 'none', class: 'hist', role: 'img', 'aria-label': `${fmtNum(total)} ${unit} over time` });
  slots.forEach(([t, c], i) => {
    const bh = c ? Math.max(2, ((H - padB) * c) / max) : 0;
    const bar = el('rect', { x: i * bw + 0.5, y: H - padB - bh, width: Math.max(1, bw - 1), height: bh, class: 'bar' });
    bar.append(el('title', {}, `${fmtTime(t)}: ${fmtNum(c)}`));
    svg.append(bar);
  });
  svg.append(el('line', { x1: 0, x2: W, y1: H - padB, y2: H - padB, class: 'axis' }));
  const wrap = document.createElement('div');
  wrap.className = 'hist-wrap';
  const axis = document.createElement('div');
  axis.className = 'hist-axis muted';
  axis.append(...[fmtTime(from), `${fmtNum(total)} ${unit} · bucket ${step >= 60000 ? `${step / 60000} min` : `${step / 1000} s`}`, fmtTime(to)].map((t) => Object.assign(document.createElement('span'), { textContent: t })));
  wrap.append(svg, axis);
  return wrap;
}
