// Data health (R-I2): is data arriving, how fast, and which services went quiet.

import * as api from '../api.js';
import { histogram } from '../components/histogram.js';
import { stat } from '../components/stat.js';
import { dataTable, stateBox } from '../components/table.js';
import { href } from '../router.js';
import { timeWindow } from '../state.js';
import { h, mount } from '../util/dom.js';
import { fmtAge, fmtDuration, fmtNum, fmtTime } from '../util/format.js';

const STATE = { ok: ['ok', 'Receiving data'], quiet: ['warn', 'Quiet'], empty: ['', 'No data'] };
const badge = (s) => h('span', { class: `badge ${STATE[s]?.[0] ?? ''}` }, STATE[s]?.[1] ?? s);

export async function healthPage(view) {
  view.append(stateBox('Checking streams…'));
  const w = timeWindow();
  const { data: f, meta } = await api.get('ingest/freshness');
  const cards = h('div', { class: 'grid' }, f.streams.map((s) =>
    h('div', { class: 'card stat' },
      h('span', { class: 'label' }, `${s.stream} stream`),
      h('span', { class: 'value' }, s.age_secs == null ? '–' : `${fmtDuration(s.age_secs)} ago`),
      h('div', null, badge(s.state)),
      h('span', { class: 'hint' }, s.newest_ms ? `newest row ${fmtTime(s.newest_ms)}` : `nothing in the last ${f.lookback_hours} h`))));

  const quiet = f.streams.filter((s) => s.state === 'quiet');
  const empty = f.streams.every((s) => s.state === 'empty');
  const banner = empty
    ? h('div', { class: 'callout warn' }, h('strong', null, 'No data yet. '), 'Nothing arrived in the last ', `${f.lookback_hours} h. See `, h('a', { href: href('connect') }, 'Connect'), ' to send telemetry.')
    : quiet.length
      ? h('div', { class: 'callout warn' }, h('strong', null, `${quiet.map((s) => s.stream).join(' and ')} ${quiet.length > 1 ? 'have' : 'has'} gone quiet. `), `No new rows for more than ${fmtDuration(f.quiet_secs)}. Check the collector or SDK that writes it.`)
      : null;

  const rateBox = (stream) => h('div', { class: 'card' }, h('h2', null, `Rows per bucket: ${stream}`), stateBox('Counting…'));
  const boxes = { logs: rateBox('logs'), traces: rateBox('traces') };
  const svcBox = h('div', { class: 'card' }, h('h2', null, 'Services: last seen'), stateBox('Loading…'));

  mount(view,
    h('div', { class: 'page-head' }, h('h2', null, 'Data health'), h('span', { class: 'sub' }, `quiet after ${fmtDuration(f.quiet_secs)} without a new row`)),
    banner, cards, h('div', { class: 'grid2' }, boxes.logs, boxes.traces), svcBox);

  for (const stream of ['logs', 'traces']) {
    api.get('ingest/rate', { from: w.from, to: w.to, stream }).then(({ data }) =>
      mount(boxes[stream], h('h2', null, `Rows per bucket: ${stream}`), data.total ? [histogram(data, w.from, w.to, 'rows'), h('p', { class: 'muted' }, `${fmtNum(data.total)} rows, about ${data.per_minute < 10 ? data.per_minute.toFixed(1) : fmtNum(Math.round(data.per_minute))} per minute`)] : h('div', { class: 'empty' }, 'No rows in this time range.')),
    ).catch((e) => mount(boxes[stream], h('h2', null, `Rows per bucket: ${stream}`), stateBox(e.message, 'error')));
  }
  api.get('ingest/services', { from: w.from, to: w.to }).then(({ data }) =>
    mount(svcBox, h('h2', null, 'Services: last seen'), dataTable({
      rows: data, empty: 'No spans in this time range.',
      columns: [
        { key: 'service', label: 'Service' },
        { key: 'state', label: 'State', render: (r) => badge(r.state) },
        { key: 'last_seen_ms', label: 'Last span', render: (r) => `${fmtTime(r.last_seen_ms)} (${fmtAge(r.last_seen_ms)})` },
        { key: 'spans', label: 'Spans in range', num: true, render: (r) => fmtNum(r.spans) },
      ] })),
  ).catch((e) => mount(svcBox, h('h2', null, 'Services: last seen'), stateBox(e.message, 'error')));
  void meta;
}
