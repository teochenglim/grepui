// One trace: summary, then Waterfall | Flame graph | Logs, with a span panel.

import * as api from '../api.js';
import { flameGraph } from '../components/flamegraph.js';
import { logTable } from '../components/logtable.js';
import { feedbackBox, fmtCost, kindBadge } from '../components/llmspan.js';
import { serviceMap } from '../components/servicemap.js';
import { spanPanel } from '../components/spanpanel.js';
import { dataTable, stateBox } from '../components/table.js';
import { waterfall } from '../components/waterfall.js';
import { currentQuery, href } from '../router.js';
import { h, mount } from '../util/dom.js';
import { fmtNum, fmtTime, fmtUs, serviceColor } from '../util/format.js';
import { roleIn, state } from '../state.js';

/** The window to search: around `?at=` when we came from a list, else the server default (7 days). */
function traceWindow() {
  const at = Number(currentQuery().get('at'));
  return at > 0 ? { from: at - 5 * 60e3, to: at + 2 * 3600e3 } : {};
}

/** The LLM steps of the trace in time order: one row per span with an LLM view. */
const stepsTable = (spans, onPick) => dataTable({
  rows: spans.filter((x) => x.llm), empty: 'No LLM spans.', onRow: onPick,
  columns: [
    { key: 'offset_us', label: '+', render: (x) => fmtUs(x.offset_us) },
    { key: 'name', label: 'Step', render: (x) => h('span', { style: `padding-left:${x.depth * 10}px` }, kindBadge(x.llm.kind), ' ', x.name) },
    { key: 'model', label: 'Model', render: (x) => x.llm.model || x.llm.tool?.name || '' },
    { key: 'tokens', label: 'Tokens', num: true, render: (x) => (x.llm.tokens?.total != null ? fmtNum(x.llm.tokens.total) : '') },
    { key: 'cost', label: 'Cost', num: true, render: (x) => fmtCost(x.llm.cost?.total, x.llm.cost?.estimated) || '' },
    { key: 'duration_us', label: 'Duration', num: true, render: (x) => fmtUs(x.duration_us) },
    { key: 'error', label: '', render: (x) => (x.error ? h('span', { class: 'badge bad' }, 'error') : '') }] });

const kvRow = (k, v) => h('div', { class: 'kv' }, h('code', null, k), h('span', null, v));

/** The side panel for a selected flame-graph node. */
function flameDetails(n) {
  return h('div', { class: 'card' },
    h('h2', null, n.name),
    kvRow('service', n.service || '(trace)'),
    kvRow('total', fmtUs(n.total_us)),
    kvRow('self', fmtUs(n.self_us)),
    kvRow('spans merged', fmtNum(n.count)),
    n.errors ? kvRow('errors', fmtNum(n.errors)) : null);
}

export async function traceDetailPage(view, { id }) {
  view.append(stateBox('Loading trace…'));
  const win = traceWindow();
  const { data } = await api.get(`traces/${encodeURIComponent(id)}`, win);
  const { trace, spans } = data;
  const panel = h('div', { class: 'panel-slot' }, h('div', { class: 'card muted' }, 'Select a span to see its attributes, events and links.'));
  const body = h('div');
  let tab = 'waterfall';
  let flame = null;
  let logs = null;

  const canWrite = ['editor', 'admin'].includes(roleIn(state.org));
  let feedback = [];
  const loadFeedback = async () => { try { feedback = (await api.get('llm/feedback', { trace: id })).data; } catch { feedback = []; } };
  const sendFeedback = async (span, body) => { try { await api.send('PUT', 'llm/feedback', { trace_id: id, span_id: span, ...body }); await loadFeedback(); paintFeedback(); if (selected) select(selected); } catch (e) { alert(e.message); } };
  const removeFeedback = async (span) => { try { await api.send('DELETE', `llm/feedback?trace=${id}${span ? `&span=${span}` : ''}`); await loadFeedback(); paintFeedback(); if (selected) select(selected); } catch (e) { alert(e.message); } };
  let selected = null;
  const select = (s) => {
    selected = s;
    mount(panel, spanPanel(s, {
      onLogs: (sp) => showLogs(sp.span_id),
      feedback: s.llm ? { items: feedback.filter((f) => f.span_id === s.span_id), canWrite, onSend: (b) => sendFeedback(s.span_id, b), onRemove: () => removeFeedback(s.span_id) } : null,
    }));
  };
  const feedbackSlot = h('div', { class: 'card' });
  const paintFeedback = () => mount(feedbackSlot, h('h3', null, 'Feedback on this trace'), feedbackBox({ items: feedback.filter((f) => !f.span_id), canWrite, onSend: (b) => sendFeedback(null, b), onRemove: () => removeFeedback(null) }));
  let agent = null;

  async function showLogs(spanId) {
    tab = 'logs';
    paintTabs();
    mount(body, stateBox('Loading logs…'));
    try {
      const rows = (await api.get(`traces/${encodeURIComponent(id)}/logs`, { ...win, span: spanId || null })).data;
      logs = rows;
      mount(body, spanId ? h('p', { class: 'muted' }, `Logs of span ${spanId}. `, h('a', { href: '#', onclick: (e) => (e.preventDefault(), (logs = null), showLogs(null)) }, 'Show the whole trace')) : null, logTable(rows));
    } catch (e) {
      mount(body, stateBox(e.message, 'error'));
    }
  }

  async function paintBody() {
    if (tab === 'waterfall') mount(body, waterfall(spans, trace.duration_us, select));
    else if (tab === 'steps') mount(body, stepsTable(spans, async (x) => { tab = 'waterfall'; paintTabs(); await paintBody(); select(x); }));
    else if (tab === 'graph') {
      mount(body, stateBox('Building the graph…'));
      try {
        agent ||= (await api.get(`traces/${encodeURIComponent(id)}/agent-graph`, win)).data;
        const m = agent.map;
        const info = new Map(agent.nodes.map((n) => [n.id, n]));
        const drawn = { ...m, nodes: m.nodes.map((n) => ({ ...n, label: (info.get(n.id) || {}).label })) };
        mount(body,
          h('p', { class: 'muted' }, 'Agents, tools and model calls. An arrow means the left step ran the right one; width follows the call count.'),
          m.nodes.length ? serviceMap(drawn, () => {}) : h('div', { class: 'empty' }, 'No edges: this trace has one step or none.'),
          dataTable({ rows: agent.nodes, empty: 'No LLM steps in this trace.', columns: [
            { key: 'label', label: 'Step', render: (n) => h('span', null, kindBadge(n.kind), ' ', n.label) }, { key: 'calls', label: 'Calls', num: true },
            { key: 'errors', label: 'Errors', num: true }, { key: 'p95_us', label: 'p95', num: true, render: (n) => fmtUs(n.p95_us) },
            { key: 'tokens', label: 'Tokens', num: true, render: (n) => (n.tokens ? fmtNum(n.tokens) : '') }, { key: 'cost', label: 'Cost', num: true, render: (n) => (n.cost ? fmtCost(n.cost) : '') }] }));
      } catch (e) {
        mount(body, stateBox(e.message, 'error'));
      }
    }
    else if (tab === 'flame') {
      mount(body, stateBox('Merging call paths…'));
      try {
        flame ||= (await api.get(`traces/${encodeURIComponent(id)}/flame`, win)).data.root;
        mount(
          body,
          h('p', { class: 'muted' }, 'Identical call paths are merged. Width is total time; hover for self time.'),
          flameGraph(flame, (n) => mount(panel, flameDetails(n))),
        );
      } catch (e) {
        mount(body, stateBox(e.message, 'error'));
      }
    } else if (logs) mount(body, logTable(logs));
    else showLogs(null);
  }

  const tabs = h('div', { class: 'tabs', role: 'tablist' });
  function paintTabs() {
    mount(tabs, [['waterfall', 'Waterfall'], ...(trace.llm ? [['steps', 'LLM steps'], ['graph', 'Agent graph']] : []), ['flame', 'Flame graph'], ['logs', 'Logs']].map(([k, l]) => h('button', { class: 'tab', role: 'tab', 'aria-selected': String(k === tab), onclick: () => { tab = k; paintTabs(); paintBody(); } }, l)));
  }

  const first = spans[0] || {};
  mount(view,
    h('div', { class: 'page-head' }, h('a', { href: href('traces') }, '← Traces'), h('h2', null, `${first.service || ''} ${first.name || ''}`), h('span', { class: 'sub mono' }, trace.trace_id)),
    h('div', { class: 'row' },
      h('span', { class: 'badge' }, `${fmtUs(trace.duration_us)}`), h('span', { class: 'badge' }, `${fmtNum(trace.spans)} spans`),
      h('span', { class: 'badge' }, `${trace.services.length} services`), trace.errors ? h('span', { class: 'badge bad' }, `${trace.errors} errors`) : h('span', { class: 'badge ok' }, 'no errors'),
      h('span', { class: 'muted' }, fmtTime(trace.start_ms)),
      ...trace.services.map((s) => h('span', { class: 'chip-static' }, h('span', { class: 'dot', style: `background:${serviceColor(s)}` }), s))),
    trace.llm ? h('div', { class: 'row wrap' }, kindBadge('LLM'), h('span', { class: 'badge' }, `${trace.llm.llm_calls} model calls`), h('span', { class: 'badge' }, `${fmtNum(trace.llm.total_tokens)} tokens`), trace.llm.cost ? h('span', { class: 'badge' }, fmtCost(trace.llm.cost, trace.llm.cost_estimated)) : null, ...trace.llm.models.map((m) => h('span', { class: 'chip-static' }, m)), trace.llm.tools.length ? h('span', { class: 'muted' }, `tools: ${trace.llm.tools.join(', ')}`) : null, trace.llm.session ? h('span', { class: 'muted' }, `session ${trace.llm.session}`) : null, trace.llm.user ? h('span', { class: 'muted' }, `user ${trace.llm.user}`) : null) : null,
    trace.llm ? feedbackSlot : null,
    trace.truncated ? h('div', { class: 'callout warn' }, 'This trace has more spans than the 5,000 shown.') : null,
    tabs,
    h('div', { class: 'split' }, h('div', { class: 'grow' }, body), h('div', { class: 'side-col' }, panel)));
  paintTabs();
  paintBody();
  if (trace.llm) loadFeedback().then(paintFeedback);
}
