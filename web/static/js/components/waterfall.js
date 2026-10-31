// The trace timeline: one row per span, indented by depth, bars positioned by offset.

import { h } from '../util/dom.js';
import { fmtUs, serviceColor } from '../util/format.js';
import { kindBadge } from './llmspan.js';

export function waterfall(spans, totalUs, onSelect) {
  const total = Math.max(1, totalUs);
  const ticks = [0, 0.25, 0.5, 0.75, 1].map((f) => h('span', { style: `left:${f * 100}%` }, fmtUs(total * f)));
  const rows = spans.map((s) => {
    const left = Math.min(99.5, (s.offset_us / total) * 100);
    const width = Math.max(0.4, Math.min(100 - left, (s.duration_us / total) * 100));
    const row = h('div', { class: `wf-row${s.error ? ' err' : ''}`, tabindex: '0', role: 'button', 'aria-label': `${s.service} ${s.name} ${fmtUs(s.duration_us)}` },
      h('div', { class: 'wf-label', style: `padding-left:${s.depth * 14 + 6}px` },
        h('span', { class: 'dot', style: `background:${serviceColor(s.service)}` }),
        h('strong', null, s.service), kindBadge(s.llm && s.llm.kind), h('span', { class: 'muted clip' }, s.name)),
      h('div', { class: 'wf-track' },
        h('div', { class: 'wf-bar', style: `left:${left}%;width:${width}%;background:${s.error ? 'var(--bad)' : serviceColor(s.service)}` }),
        h('span', { class: 'wf-dur', style: left > 70 ? `right:${100 - left}%` : `left:${left + width}%` }, fmtUs(s.duration_us))));
    const pick = () => {
      document.querySelectorAll('.wf-row.sel').forEach((r) => r.classList.remove('sel'));
      row.classList.add('sel');
      onSelect(s);
    };
    row.addEventListener('click', pick);
    row.addEventListener('keydown', (e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), pick()));
    return row;
  });
  return h('div', { class: 'waterfall' }, h('div', { class: 'wf-head' }, h('div', { class: 'wf-label muted' }, 'Service / operation'), h('div', { class: 'wf-track wf-ticks' }, ticks)), rows);
}
