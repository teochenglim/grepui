// One trace: summary, then Waterfall | Flame graph | Logs, with a span panel.

import * as api from '../api.js';
import { flameGraph } from '../components/flamegraph.js';
import { logTable } from '../components/logtable.js';
import { spanPanel } from '../components/spanpanel.js';
import { stateBox } from '../components/table.js';
import { waterfall } from '../components/waterfall.js';
import { currentQuery, href } from '../router.js';
import { h, mount } from '../util/dom.js';
import { fmtNum, fmtTime, fmtUs, serviceColor } from '../util/format.js';

/** The window to search: around `?at=` when we came from a list, else the server default (7 days). */
function traceWindow() {
  const at = Number(currentQuery().get('at'));
  return at > 0 ? { from: at - 5 * 60e3, to: at + 2 * 3600e3 } : {};
}

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

  const select = (s) => mount(panel, spanPanel(s, { onLogs: (sp) => showLogs(sp.span_id) }));

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
    mount(tabs, [['waterfall', 'Waterfall'], ['flame', 'Flame graph'], ['logs', 'Logs']].map(([k, l]) => h('button', { class: 'tab', role: 'tab', 'aria-selected': String(k === tab), onclick: () => { tab = k; paintTabs(); paintBody(); } }, l)));
  }

  const first = spans[0] || {};
  mount(view,
    h('div', { class: 'page-head' }, h('a', { href: href('traces') }, '← Traces'), h('h2', null, `${first.service || ''} ${first.name || ''}`), h('span', { class: 'sub mono' }, trace.trace_id)),
    h('div', { class: 'row' },
      h('span', { class: 'badge' }, `${fmtUs(trace.duration_us)}`), h('span', { class: 'badge' }, `${fmtNum(trace.spans)} spans`),
      h('span', { class: 'badge' }, `${trace.services.length} services`), trace.errors ? h('span', { class: 'badge bad' }, `${trace.errors} errors`) : h('span', { class: 'badge ok' }, 'no errors'),
      h('span', { class: 'muted' }, fmtTime(trace.start_ms)),
      ...trace.services.map((s) => h('span', { class: 'chip-static' }, h('span', { class: 'dot', style: `background:${serviceColor(s)}` }), s))),
    trace.truncated ? h('div', { class: 'callout warn' }, 'This trace has more spans than the 5,000 shown.') : null,
    tabs,
    h('div', { class: 'split' }, h('div', { class: 'grow' }, body), h('div', { class: 'side-col' }, panel)));
  paintTabs();
  paintBody();
}
