// Flame graph: identical call paths merged; width is total time, the rest is self time.

import { h } from '../util/dom.js';
import { fmtNum, fmtUs, serviceColor } from '../util/format.js';

export function flameGraph(root, onSelect) {
  const total = Math.max(1, root.total_us);
  const nodes = [];
  let depth = 0;
  (function place(n, x, level) {
    nodes.push({ n, x, w: (n.total_us / total) * 100, level });
    depth = Math.max(depth, level);
    let cx = x;
    for (const c of n.children) (place(c, cx, level + 1), (cx += (c.total_us / total) * 100));
  })(root, 0, 0);
  const box = h('div', { class: 'flame', style: `height:${(depth + 1) * 24 + 4}px` });
  for (const { n, x, w, level } of nodes) {
    if (w < 0.15) continue;
    const el = h('div', {
      class: `flame-node${n.errors ? ' err' : ''}`, tabindex: '0', role: 'button',
      style: `left:${x}%;width:${w}%;top:${level * 24}px;background:${n.errors ? 'var(--bad)' : serviceColor(n.service)}`,
      title: `${n.service ? `${n.service} · ` : ''}${n.name}\ntotal ${fmtUs(n.total_us)} · self ${fmtUs(n.self_us)} · ${fmtNum(n.count)} span${n.count === 1 ? '' : 's'}${n.errors ? ` · ${n.errors} errors` : ''}`,
    }, h('span', null, `${n.name}`));
    const pick = () => onSelect(n);
    el.addEventListener('click', pick);
    el.addEventListener('keydown', (e) => e.key === 'Enter' && pick());
    box.append(el);
  }
  return box;
}
