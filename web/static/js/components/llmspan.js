// The LLM view of one span: kind, model, token and cost chips, the conversation, tool calls,
// retrieved documents, embeddings (text only) and evaluations. The API builds the view; this draws it.

import { h } from '../util/dom.js';
import { fmtNum } from '../util/format.js';

export const KIND_CLASS = { LLM: 'k-llm', AGENT: 'k-agent', TOOL: 'k-tool', CHAIN: 'k-chain', RETRIEVER: 'k-retr', RERANKER: 'k-retr', EMBEDDING: 'k-emb', EVALUATOR: 'k-eval', GUARDRAIL: 'k-eval' };
export const kindBadge = (kind) => (kind ? h('span', { class: `badge kind ${KIND_CLASS[kind] || ''}` }, kind) : null);
export const fmtCost = (c, estimated) => (typeof c === 'number' ? `${estimated ? '≈ ' : ''}$${c < 0.01 ? c.toFixed(5) : c.toFixed(3)}` : null);

/** Text longer than a few lines folds; the size says how much is behind it. */
export function longText(text, { max = 600, cls = '' } = {}) {
  const t = String(text ?? '');
  if (t.length <= max) return h('div', { class: `llm-text ${cls}` }, t);
  return h('details', { class: `llm-text ${cls}` }, h('summary', null, `${t.slice(0, 160).replace(/\s+/g, ' ')}… (${fmtNum(t.length)} characters)`), h('div', null, t));
}

const chip = (label, value) => (value == null || value === '' ? null : h('span', { class: 'chip-static' }, h('span', { class: 'muted' }, `${label} `), value));

function message(m) {
  const body = [];
  if (m.content != null) body.push(longText(m.content));
  for (const c of m.tool_calls || []) body.push(h('div', { class: 'tool-call' }, h('strong', null, `→ ${c.name || 'tool'}`), c.arguments ? h('code', null, ` ${c.arguments}`) : null));
  if (!body.length) body.push(h('span', { class: 'muted' }, '(no text)'));
  return h('div', { class: `msg role-${m.role || 'none'}` }, h('div', { class: 'msg-role' }, m.role || '?', m.name ? ` · ${m.name}` : '', m.tool_call_id ? ` · ${m.tool_call_id}` : ''), body);
}

export function llmSection(v) {
  if (!v) return null;
  const t = v.tokens || {};
  const parts = [
    h('div', { class: 'row' }, kindBadge(v.kind), v.model ? h('strong', null, v.model) : null, v.provider ? h('span', { class: 'muted' }, v.provider) : null, v.finish_reason ? h('span', { class: 'badge' }, v.finish_reason) : null),
  ];
  const chips = [
    chip('prompt', t.prompt != null ? fmtNum(t.prompt) : null), chip('completion', t.completion != null ? fmtNum(t.completion) : null), chip('total', t.total != null ? fmtNum(t.total) : null),
    chip('cache read', t.cache_read != null ? fmtNum(t.cache_read) : null), chip('reasoning', t.reasoning != null ? fmtNum(t.reasoning) : null),
    chip('cost', fmtCost(v.cost?.total, v.cost?.estimated)), chip('session', v.session), chip('user', v.user), chip('agent', v.agent),
  ].filter(Boolean);
  if (chips.length) parts.push(h('div', { class: 'row wrap' }, chips));
  const msgs = v.messages || {};
  if ((msgs.input || []).length) parts.push(h('h3', null, `Conversation (${msgs.input.length})`), h('div', { class: 'transcript' }, msgs.input.map(message)));
  if ((msgs.output || []).length) parts.push(h('h3', null, 'Response'), h('div', { class: 'transcript' }, msgs.output.map(message)));
  if (v.tool) parts.push(h('h3', null, `Tool ${v.tool.name || ''}`), v.tool.description ? h('p', { class: 'muted' }, v.tool.description) : null, v.tool.arguments ? h('div', { class: 'kv' }, h('code', null, 'arguments'), longText(v.tool.arguments, { cls: 'mono' })) : null, v.tool.result ? h('div', { class: 'kv' }, h('code', null, 'result'), longText(v.tool.result)) : null);
  const docs = (v.documents || []).length ? v.documents : v.reranked || [];
  if (docs.length) parts.push(h('h3', null, `Documents (${docs.length})`), docs.map((d) => h('div', { class: 'doc' }, h('div', { class: 'row' }, h('strong', null, d.id || '(no id)'), typeof d.score === 'number' ? h('span', { class: 'badge' }, d.score.toFixed(3)) : null, d.url ? h('span', { class: 'muted' }, d.url) : null), d.content != null ? longText(d.content, { max: 300 }) : null)));
  if ((v.embeddings || []).length) parts.push(h('h3', null, `Embeddings (${v.embeddings.length})`), v.embeddings.map((e) => h('div', { class: 'doc' }, e.text != null ? longText(e.text, { max: 300 }) : null, e.dims ? h('span', { class: 'muted' }, `${e.dims} dimensions (vector not shown)`) : null)));
  if (v.input?.value != null || v.output?.value != null) {
    parts.push(h('h3', null, 'Input and output'));
    if (v.input?.value != null) parts.push(h('div', { class: 'kv' }, h('code', null, 'input'), longText(v.input.value)));
    if (v.output?.value != null) parts.push(h('div', { class: 'kv' }, h('code', null, 'output'), longText(v.output.value)));
  }
  if ((v.evaluations || []).length) parts.push(h('h3', null, 'Evaluations'), v.evaluations.map((e) => h('div', { class: 'row wrap' }, h('strong', null, e.name || '(unnamed)'), e.label ? h('span', { class: 'badge' }, e.label) : null, typeof e.score === 'number' ? h('span', { class: 'badge' }, String(e.score)) : null, e.source ? h('span', { class: 'muted' }, e.source) : null, e.explanation ? longText(e.explanation, { max: 200 }) : null)));
  if (v.template?.template) parts.push(h('h3', null, 'Prompt template'), longText(v.template.template, { cls: 'mono' }));
  if (v.parameters) parts.push(h('div', { class: 'kv' }, h('code', null, 'parameters'), h('span', { class: 'mono' }, v.parameters)));
  return h('section', { class: 'llm-view', 'aria-label': 'LLM view' }, parts);
}

/** Feedback on a trace or span: a thumb, a note, and what people already said. */
export function feedbackBox({ items, onSend, onRemove, canWrite }) {
  const note = h('input', { placeholder: 'note (optional)', 'aria-label': 'Feedback note', maxlength: '2000' });
  const send = (rating) => onSend({ rating, note: note.value });
  return h('div', { class: 'feedback' },
    items.length ? items.map((f) => h('div', { class: 'row wrap' }, h('span', { class: `badge ${f.rating === 'up' ? 'ok' : f.rating === 'down' ? 'bad' : ''}` }, f.rating || 'note'), typeof f.score === 'number' ? h('span', { class: 'badge' }, String(f.score)) : null, f.label ? h('span', { class: 'badge' }, f.label) : null, h('span', null, f.note || ''), h('span', { class: 'muted' }, f.author))) : h('span', { class: 'muted' }, 'No feedback yet.'),
    canWrite ? h('div', { class: 'row' }, note, h('button', { type: 'button', onclick: () => send('up'), 'aria-label': 'Good' }, '👍'), h('button', { type: 'button', onclick: () => send('down'), 'aria-label': 'Bad' }, '👎'), h('button', { type: 'button', class: 'ghost', onclick: onRemove }, 'Remove mine')) : null);
}
