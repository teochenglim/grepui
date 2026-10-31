// LLM (R-AI5, R-AI6): calls, tokens, cost, errors and latency by model, provider, span kind and tool;
// sessions and users. Reads the org's trace v2 table; the filter bar is the one the Traces page has.

import * as api from '../api.js';
import { fmtCost, kindBadge, longText } from '../components/llmspan.js';
import { histogram } from '../components/histogram.js';
import { lineChart } from '../components/linechart.js';
import { dataTable, stateBox } from '../components/table.js';
import { traceHref } from './traces.js';
import { currentQuery, replaceQuery } from '../router.js';
import { timeWindow } from '../state.js';
import { h, mount } from '../util/dom.js';
import { fmtNum, fmtTime, fmtUs } from '../util/format.js';

const TABS = [['overview', 'Overview'], ['model', 'Models'], ['provider', 'Providers'], ['kind', 'Span kinds'], ['tool', 'Tools'], ['sessions', 'Sessions'], ['users', 'Users']];
const stat = (label, value, note) => h('div', { class: 'card stat' }, h('div', { class: 'label' }, label), h('div', { class: 'value' }, value), note ? h('div', { class: 'hint' }, note) : null);
const ms = (v) => (v == null ? '–' : v >= 1000 ? `${(v / 1000).toFixed(2)} s` : `${v < 10 ? v.toFixed(1) : Math.round(v)} ms`);
const pct = (v) => `${Number(v || 0).toFixed(1)}%`;
const cost = (c) => (c ? fmtCost(c.total, c.estimated > 0) : '–');
const age = (t) => (t ? fmtTime(t) : '–');

export async function llmPage(view, _p, tab = 'overview') {
  const qp = currentQuery();
  const st = { q: qp.get('q') || '', model: qp.get('model') || '' };
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  const q = h('input', { type: 'search', class: 'grow', placeholder: 'Filter spans: llm.model_name=gpt-4o user.id=u1 status=error', 'aria-label': 'Filter', value: st.q, spellcheck: 'false' });
  const run = h('button', { class: 'primary', type: 'button' }, 'Run');
  const go = () => { st.q = q.value.trim(); replaceQuery({ q: st.q || null }); load(); };
  run.addEventListener('click', go);
  q.addEventListener('keydown', (e) => e.key === 'Enter' && go());
  mount(view,
    h('div', { class: 'page-head' }, h('h2', null, 'LLM'), h('span', { class: 'sub' }, 'model calls from OpenInference and GenAI traces')),
    h('div', { class: 'filterbar' }, h('div', { class: 'row' }, q, run)),
    h('div', { class: 'tabs', role: 'tablist' }, TABS.map(([k, l]) => h('button', { type: 'button', role: 'tab', class: 'tab', 'aria-selected': k === tab, onclick: () => llmPage(view, _p, k) }, l))), out);

  const params = () => ({ ...timeWindow(), q: st.q || null });
  const breakdown = (by) => async () => {
    const { data } = await api.get('llm/breakdown', { ...params(), by });
    const first = { model: 'Model', provider: 'Provider', kind: 'Span kind', tool: 'Tool' }[by];
    mount(out, h('p', { class: 'muted' }, by === 'model' || by === 'provider' ? 'Model calls only. Cost is what spans reported, plus the price table for calls that reported none (marked ≈; Administration > LLM).' : by === 'kind' ? 'Every span that carries a kind. Tokens and cost count model calls only.' : 'Tool spans. Latency is the tool call itself.'),
      dataTable({ rows: data, empty: 'Nothing in this range for the filter.', columns: [
        { key: 'grp', label: first, render: (r) => (by === 'kind' ? kindBadge(r.grp) || r.grp : r.grp) },
        { key: 'calls', label: 'Calls', num: true, render: (r) => fmtNum(r.calls) },
        { key: 'errors', label: 'Errors', num: true, render: (r) => (r.errors ? h('span', { class: 'badge bad' }, `${fmtNum(r.errors)} · ${pct(r.error_rate)}`) : '') },
        { key: 'p50_ms', label: 'p50', num: true, render: (r) => ms(r.p50_ms) }, { key: 'p95_ms', label: 'p95', num: true, render: (r) => ms(r.p95_ms) }, { key: 'p99_ms', label: 'p99', num: true, render: (r) => ms(r.p99_ms) },
        { key: 'prompt_tokens', label: 'Prompt', num: true, render: (r) => fmtNum(r.prompt_tokens) }, { key: 'completion_tokens', label: 'Completion', num: true, render: (r) => fmtNum(r.completion_tokens) },
        { key: 'cost', label: 'Cost', num: true, render: (r) => cost(r.cost) }] }),
      h('p', { class: 'muted' }, 'Percentiles are approximate.'));
  };
  const who = (kind) => async () => {
    const { data } = await api.get(`llm/${kind}`, { ...params(), limit: 100 });
    const detail = h('div', { class: 'stack' });
    const open = async (r) => {
      mount(detail, stateBox('Loading the session…'));
      try {
        const d = (await api.get(`llm/sessions/${encodeURIComponent(r.session_id)}`, params())).data;
        mount(detail, h('div', { class: 'card stack' }, h('h3', null, `Session ${d.session_id}`), h('p', { class: 'muted' }, 'One row per trace, oldest first. Only spans that carry the session id are counted.'),
          dataTable({ rows: d.turns, empty: 'No turns.', columns: [
            { key: 'start_ms', label: 'Start', render: (t) => h('a', { class: 'mono nowrap', href: traceHref(t.trace_id, t.start_ms) }, fmtTime(t.start_ms)) },
            { key: 'input.value', label: 'Input', render: (t) => (t['input.value'] ? longText(t['input.value'], { max: 160 }) : '') }, { key: 'output.value', label: 'Output', render: (t) => (t['output.value'] ? longText(t['output.value'], { max: 160 }) : '') },
            { key: 'llm_calls', label: 'Calls', num: true }, { key: 'total_tokens', label: 'Tokens', num: true, render: (t) => fmtNum(t.total_tokens) },
            { key: 'cost', label: 'Cost', num: true, render: (t) => (t.cost ? fmtCost(t.cost) : '') }, { key: 'duration_us', label: 'Duration', num: true, render: (t) => fmtUs(t.duration_us) },
            { key: 'errors', label: '', render: (t) => (t.errors ? h('span', { class: 'badge bad' }, 'error') : '') }] })));
      } catch (e) { mount(detail, stateBox(e.message, 'error')); }
    };
    const isSessions = kind === 'sessions';
    mount(out, h('p', { class: 'muted' }, isSessions ? 'Sessions group spans by session.id (or gen_ai.conversation.id). Click one to read it turn by turn. Cost is what spans reported.' : 'Users group spans by user.id.'),
      dataTable({ rows: data, empty: isSessions ? 'No sessions: spans need a session.id attribute.' : 'No users: spans need a user.id attribute.', onRow: isSessions ? open : undefined, columns: [
        { key: isSessions ? 'session_id' : 'user_id', label: isSessions ? 'Session' : 'User', render: (r) => h('span', { class: 'mono' }, r.session_id || r.user_id) },
        ...(isSessions ? [{ key: 'user', label: 'User' }] : [{ key: 'sessions', label: 'Sessions', num: true }]),
        { key: 'traces', label: 'Traces', num: true }, { key: 'llm_calls', label: 'Calls', num: true, render: (r) => fmtNum(r.llm_calls) },
        { key: 'errors', label: 'Errors', num: true, render: (r) => (r.errors ? h('span', { class: 'badge bad' }, fmtNum(r.errors)) : '') },
        { key: 'total_tokens', label: 'Tokens', num: true, render: (r) => fmtNum(r.total_tokens) }, { key: 'cost', label: 'Cost', num: true, render: (r) => (r.cost ? fmtCost(r.cost) : '') },
        { key: 'last_ms', label: 'Last active', render: (r) => age(r.last_ms) }] }), detail);
  };

  const views = {
    async overview() {
      const p = params();
      const { data: d } = await api.get('llm/overview', p);
      const t = d.totals;
      const series = (key, scale = 1) => d.series.map((b) => ({ t: Number(b.t), v: Number(b[key]) * scale }));
      mount(out,
        h('div', { class: 'row wrap' }, stat('Model calls', fmtNum(t.calls)), stat('Tokens', fmtNum(t.total_tokens), `${fmtNum(t.prompt_tokens)} prompt · ${fmtNum(t.completion_tokens)} completion`),
          stat('Cost', cost(t.cost), t.cost.estimated > 0 ? 'part estimated from the price table' : 'as reported by the spans'), stat('Error rate', pct(t.error_rate), `${fmtNum(t.errors)} failed`),
          stat('Latency p50', ms(t.p50_ms)), stat('p95', ms(t.p95_ms)), stat('p99', ms(t.p99_ms))),
        h('div', { class: 'grid2' },
          h('div', { class: 'card' }, h('h3', null, 'Calls over time'), histogram({ buckets: d.series.map((b) => ({ t: b.t, count: b.calls })), bucket_ms: d.bucket_ms, total: t.calls }, p.from, p.to, 'calls')),
          h('div', { class: 'card' }, h('h3', null, 'Tokens over time'), lineChart({ points: series('prompt_tokens').map((x, i) => ({ t: x.t, v: x.v + Number(d.series[i].completion_tokens) })), from: p.from, to: p.to, step: d.bucket_ms, fmt: fmtNum, label: 'Tokens over time' }))),
        h('div', { class: 'grid2' },
          h('div', { class: 'card' }, h('h3', null, 'Latency p95 over time'), lineChart({ points: series('p95_ms'), from: p.from, to: p.to, step: d.bucket_ms, fmt: ms, label: 'p95 latency' })),
          h('div', { class: 'card' }, h('h3', null, 'Cost over time'), lineChart({ points: series('cost'), from: p.from, to: p.to, step: d.bucket_ms, fmt: (v) => fmtCost(v), label: 'Cost' }))),
        h('div', { class: 'card stack' }, h('h3', null, 'By model'), dataTable({ rows: d.models, empty: 'No model calls in this range.', columns: [
          { key: 'grp', label: 'Model' }, { key: 'calls', label: 'Calls', num: true, render: (r) => fmtNum(r.calls) }, { key: 'error_rate', label: 'Errors', num: true, render: (r) => (r.errors ? pct(r.error_rate) : '') },
          { key: 'p95_ms', label: 'p95', num: true, render: (r) => ms(r.p95_ms) }, { key: 'total_tokens', label: 'Tokens', num: true, render: (r) => fmtNum(r.total_tokens) }, { key: 'cost', label: 'Cost', num: true, render: (r) => cost(r.cost) }] })));
    },
    model: breakdown('model'), provider: breakdown('provider'), kind: breakdown('kind'), tool: breakdown('tool'),
    sessions: who('sessions'), users: who('users'),
  };

  async function load() {
    mount(out, stateBox('Loading…'));
    try { await views[tab](); } catch (e) { mount(out, stateBox(e.message, 'error')); }
  }
  load();
}
