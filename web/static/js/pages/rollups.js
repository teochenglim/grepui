// Rollups (R-P3): vetted Flow templates that keep per-minute RED for long windows.

import * as api from '../api.js';
import { stateBox } from '../components/table.js';
import { h, mount } from '../util/dom.js';
import { fmtAge, fmtNum } from '../util/format.js';

const DAYS = [['1', 'last day'], ['7', 'last 7 days'], ['31', 'last 31 days']];

export async function rollupsPage(view) {
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  const msg = h('div');
  mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Rollups'), h('span', { class: 'sub' }, 'per-minute RED kept by a GreptimeDB Flow, so long windows stay fast')), msg, out);
  const say = (text, kind = '') => mount(msg, text ? h('div', { class: `callout ${kind}` }, text) : null);

  async function act(label, fn) {
    say(`${label}…`);
    try {
      const r = await fn();
      say(r, 'ok');
      await load();
    } catch (e) {
      say(e.message, 'warn');
    }
  }

  async function load() {
    try {
      const { data } = await api.get('rollups');
      mount(out,
        data.admin_writes ? null : h('div', { class: 'callout warn' }, 'Changing rollups needs admin writes: start grepui with --admin-writes (or GREPUI_ADMIN_WRITES=1). Until access control arrives this flag stands in for the admin role.'),
        data.templates.map((t) => {
          const days = h('select', { 'aria-label': 'Backfill range' }, DAYS.map(([v, l]) => h('option', { value: v }, l)));
          return h('div', { class: 'card stack' },
            h('h2', null, t.title),
            h('p', { class: 'muted' }, `Source ${t.source} → table ${t.sink}, flow ${t.flow}. Counts requests, errors, total and max duration of server and consumer spans per service per minute; p95 is not kept because percentiles cannot be combined.`),
            h('div', { class: 'row' },
              h('span', { class: `badge ${t.sink_exists ? 'ok' : ''}` }, t.sink_exists ? 'sink exists' : 'no sink'),
              h('span', { class: `badge ${t.flow_exists ? 'ok' : ''}` }, t.flow_exists ? 'flow running' : 'no flow'),
              t.rows != null ? h('span', { class: 'chip-static' }, `${fmtNum(t.rows)} rows`) : null,
              t.newest_ms ? h('span', { class: 'chip-static' }, `newest ${fmtAge(t.newest_ms)}`) : null),
            h('div', { class: 'row' },
              t.sink_exists && t.flow_exists ? null : h('button', { class: 'primary', type: 'button', disabled: !data.admin_writes, onclick: () => confirm('Create the sink table and the flow?') && act('Creating', async () => (await api.send('POST', `rollups/${t.template}`)).data.next) }, 'Create rollup'),
              t.sink_exists ? [days, h('button', { type: 'button', disabled: !data.admin_writes, onclick: () => { const end = Date.now(); act('Backfilling', async () => { const { data: d } = await api.send('POST', `rollups/${t.template}/backfill?from=${end - Number(days.value) * 86400e3}&to=${end}`); return `Backfilled ${fmtNum(d.rows)} rows in ${d.chunks} chunk(s).`; }); } }, 'Backfill history')] : null,
              t.sink_exists || t.flow_exists ? h('button', { type: 'button', disabled: !data.admin_writes, onclick: () => confirm('Drop the flow and the sink table? The rollup data is deleted; raw spans are not touched.') && act('Dropping', async () => ((await api.send('DELETE', `rollups/${t.template}`)), 'Dropped.')) }, 'Drop') : null));
        }));
    } catch (e) {
      mount(out, stateBox(e.message, 'error'));
    }
  }
  load();
}
