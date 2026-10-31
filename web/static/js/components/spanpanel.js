// The selected span: status, timing, attributes, resource attributes, events and links.

import { h } from '../util/dom.js';
import { fmtUs, serviceColor } from '../util/format.js';
import { feedbackBox, llmSection } from './llmspan.js';

const rows = (obj) => Object.entries(obj || {}).map(([k, v]) => h('div', { class: 'kv' }, h('code', null, k), h('span', null, typeof v === 'object' ? JSON.stringify(v) : String(v))));

export function spanPanel(s, { onLogs, feedback } = {}) {
  const events = Array.isArray(s.events) ? s.events : [];
  const links = Array.isArray(s.links) ? s.links : [];
  return h('aside', { class: 'card span-panel', 'aria-label': 'Span details' },
    h('div', { class: 'row between' }, h('h2', null, s.name), onLogs ? h('button', { type: 'button', onclick: () => onLogs(s) }, 'Logs for this span') : null),
    h('div', { class: 'row' },
      h('span', { class: 'badge', style: `border-color:${serviceColor(s.service)}` }, s.service),
      h('span', { class: `badge ${s.error ? 'bad' : 'ok'}` }, (s.status || '').replace('STATUS_CODE_', '') || 'UNSET'),
      h('span', { class: 'badge' }, (s.kind || '').replace('SPAN_KIND_', '')),
      h('span', { class: 'muted' }, `${fmtUs(s.duration_us)} · starts +${fmtUs(s.offset_us)}`)),
    s.message ? h('div', { class: 'callout warn' }, s.message) : null,
    llmSection(s.llm),
    feedback ? [h('h3', null, 'Feedback on this span'), feedbackBox(feedback)] : null,
    h('h3', null, 'Attributes'), rows(s.attributes).length ? rows(s.attributes) : h('span', { class: 'muted' }, 'none'),
    h('h3', null, 'Resource'), rows(s.resource).length ? rows(s.resource) : h('span', { class: 'muted' }, 'none'),
    events.length ? [h('h3', null, `Events (${events.length})`), events.map((e) => h('pre', { class: 'mono' }, JSON.stringify(e, null, 2)))] : null,
    links.length ? [h('h3', null, `Links (${links.length})`), links.map((l) => h('pre', { class: 'mono' }, JSON.stringify(l)))] : null,
    h('h3', null, 'Ids'), rows({ span_id: s.span_id, parent_id: s.parent_id || '(root)', scope: s.scope }));
}
