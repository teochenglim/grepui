// Scheduled reports (R-E1): a dashboard or saved logs view, rendered to HTML or CSV and emailed on a schedule.

import * as api from '../api.js';
import { dataTable, stateBox } from '../components/table.js';
import { h, mount } from '../util/dom.js';
import { fmtTime } from '../util/format.js';

const SCHEDULES = [['0 8 * * *', 'Every day at 08:00 UTC'], ['0 8 * * 1', 'Mondays at 08:00 UTC'], ['0 * * * *', 'Every hour'], ['0 8 1 * *', 'First of the month, 08:00 UTC']];

export async function reportsPage(view) {
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  const msg = h('div');
  const say = (t, kind = '') => mount(msg, t ? h('div', { class: `callout ${kind}` }, t) : null);
  const act = async (fn) => { try { await fn(); await load(); } catch (e) { say(e.message, 'warn'); } };
  mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Reports'), h('span', { class: 'sub' }, 'scheduled by email')), msg, out);

  async function load() {
    let reports, dashboards, views;
    try {
      [reports, dashboards, views] = await Promise.all([api.get('reports'), api.get('dashboards'), api.get('views')]);
    } catch (e) { mount(out, stateBox(/migrate/.test(e.message) ? 'The state store needs `grepui migrate` before reports can be used.' : e.message, 'error')); return; }
    const targets = { dashboard: dashboards.data.filter((d) => d.kind === 'dashboard' || !d.kind).map((d) => [d.id, d.title]), view: views.data.filter((v) => v.kind === 'logs').map((v) => [v.id, v.name]) };
    const f = {
      name: h('input', { placeholder: 'name, e.g. Daily errors', 'aria-label': 'Report name' }),
      kind: h('select', { 'aria-label': 'Report on' }, [h('option', { value: 'dashboard' }, 'Dashboard'), h('option', { value: 'view' }, 'Saved logs view')]),
      target: h('select', { 'aria-label': 'Which one' }),
      format: h('select', { 'aria-label': 'Format' }, [h('option', { value: 'html' }, 'HTML email'), h('option', { value: 'csv' }, 'CSV in the email')]),
      schedule: h('input', { value: SCHEDULES[0][0], list: 'cron-presets', 'aria-label': 'Schedule (cron, UTC)', style: 'width:150px' }),
      hours: h('input', { type: 'number', min: '1', max: '744', value: '24', 'aria-label': 'Hours of data', style: 'width:90px' }),
      to: h('input', { placeholder: 'recipients, comma separated', 'aria-label': 'Recipients', class: 'grow' }),
    };
    const fill = () => mount(f.target, (targets[f.kind.value].length ? targets[f.kind.value] : [['', 'none saved yet']]).map(([v, l]) => h('option', { value: v }, l)));
    f.kind.onchange = fill;
    fill();
    const preview = h('div');
    mount(out,
      dataTable({
        columns: [
          { key: 'name', label: 'Report', render: (r) => r.report?.name || r.id },
          { key: 'what', label: 'Of', render: (r) => `${r.report?.kind} · ${r.report?.format}` },
          { key: 'schedule', label: 'Schedule (UTC)', render: (r) => r.report?.schedule },
          { key: 'to', label: 'To', render: (r) => (r.report?.recipients || []).join(', ') },
          { key: 'state', label: 'Last run', render: (r) => (r.state?.outcome ? h('span', { class: `badge ${r.state.outcome === 'ok' ? 'ok' : r.state.outcome === 'failed' ? 'bad' : 'warn'}`, title: r.state.error || '' }, `${r.state.outcome} ${r.state.last_run_ms ? fmtTime(r.state.last_run_ms) : ''}`) : h('span', { class: 'muted' }, 'not yet')) },
          { key: 'x', label: '', render: (r) => h('span', { class: 'row' },
            h('button', { type: 'button', class: 'ghost', onclick: async () => { try { const p = (await api.get(`reports/${r.id}/preview`)).data; mount(preview, h('div', { class: 'card stack' }, h('strong', null, p.subject), p.format === 'html' ? h('iframe', { sandbox: '', srcdoc: p.body, class: 'frame', style: 'width:100%;height:420px;border:1px solid var(--line)', title: 'Report preview' }) : h('pre', { class: 'sqlbox' }, p.body))); } catch (e) { say(e.message, 'warn'); } } }, 'Preview'),
            h('button', { type: 'button', class: 'ghost', onclick: () => act(async () => { await api.send('POST', `reports/${r.id}/run`, {}); say(`Sent "${r.report.name}" to ${r.report.recipients.length} recipient(s).`, 'ok'); }) }, 'Send now'),
            h('button', { type: 'button', class: 'ghost', onclick: () => confirm(`Delete report "${r.report?.name}"?`) && act(() => api.send('DELETE', `reports/${r.id}`)) }, 'Delete')) },
        ],
        rows: reports.data, empty: 'No reports yet.',
      }),
      preview,
      h('div', { class: 'card stack' }, h('h3', null, 'New report'),
        h('datalist', { id: 'cron-presets' }, SCHEDULES.map(([v, l]) => h('option', { value: v }, l))),
        h('div', { class: 'row' }, f.name, f.kind, f.target, f.format),
        h('div', { class: 'row' }, h('label', { class: 'row' }, 'Schedule', f.schedule), h('label', { class: 'row' }, 'Last', f.hours, 'hours'), f.to),
        h('div', { class: 'row' }, h('button', { class: 'primary', type: 'button', onclick: () => act(async () => {
          await api.send('POST', 'reports', { name: f.name.value, kind: f.kind.value, target: f.target.value, format: f.format.value, schedule: f.schedule.value.trim(), range_hours: Number(f.hours.value) || 24, recipients: f.to.value.split(',').map((x) => x.trim()).filter(Boolean) });
          say('Report saved. It is sent at its next scheduled time.', 'ok');
        }) }, 'Save report'), h('span', { class: 'muted' }, 'Needs --smtp on the server. Data is masked by the org redaction rules as a viewer sees it. PDF is not supported.'))));
  }
  load();
}
