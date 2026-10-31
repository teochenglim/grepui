// The alert editor: a form that builds the rule JSON, previews it without firing, and saves.

import * as api from '../api.js';
import { dataTable, stateBox } from '../components/table.js';
import { href, navigate } from '../router.js';
import { h, mount } from '../util/dom.js';

const OPS = ['>', '>=', '<', '<=', '==', '!='];
const lines = (s) => s.split('\n').map((x) => x.trim()).filter(Boolean);

export async function alertEditPage(view, { id }) {
  const isNew = id === 'new';
  const [{ data: dests }, { data: info }, saved] = await Promise.all([api.get('destinations'), api.get('alerts/info'), isNew ? null : api.get(`alerts/${id}`)]);
  const s = saved ? saved.data.spec : { kind: 'scheduled', enabled: true, query: { kind: 'sql', text: '' }, condition: { op: '>', threshold: 0 }, schedule: { every_s: 60 }, lookback_s: 300, cooldown_s: 0, destinations: [], dedup: { fields: [], window_s: 0 }, template: { subject: '', body: '' } };
  const b = s.query?.builder || { source: 'logs', agg: 'count', field: '', filters: [], group_by: [] };
  const f = {};
  const field = (key, label, el) => ((f[key] = el), h('label', { class: 'field' }, h('span', null, label), el));
  const inp = (v, attrs = {}) => h('input', { value: v ?? '', ...attrs });
  const sel = (opts, v) => h('select', null, opts.map((o) => h('option', { value: o, selected: o === v || null }, o)));

  const preview = h('div', { class: 'stack' });
  const msg = h('div');
  const say = (t, kind = '') => mount(msg, t ? h('div', { class: `callout ${kind}` }, t) : null);
  const kind = sel(['scheduled', 'realtime', 'composite', 'anomaly'], s.kind);
  const qkind = sel(['sql', 'promql', 'builder', 'llm'], s.query?.kind || 'sql');
  const sections = h('div', { class: 'stack' });

  const els = {
    name: inp(s.name, { 'aria-label': 'Name' }), folder: inp(s.folder), desc: inp(s.description),
    text: h('textarea', { rows: 5, spellcheck: 'false', 'aria-label': 'Query' }, s.query?.text || ''),
    bsource: sel(['logs', 'traces'], b.source), bagg: sel(['count', 'avg', 'min', 'max', 'sum'], b.agg), bfield: inp(b.field),
    bfilters: h('textarea', { rows: 3, placeholder: 'column=value, one per line' }, (b.filters || []).map((x) => `${x.field}=${x.value}`).join('\n')), bgroup: inp((b.group_by || []).join(', ')),
    lmetric: sel(['cost', 'tokens', 'calls', 'error_rate', 'latency_p95'], s.query?.llm?.metric || 'cost'), lmodel: inp(s.query?.llm?.model || '', { placeholder: 'any model' }), lper: h('input', { type: 'checkbox', checked: s.query?.llm?.per_model ? true : null }),
    op: sel(OPS, s.condition?.op || '>'), thr: inp(s.condition?.threshold ?? 0, { type: 'number', step: 'any' }),
    cmpOn: h('input', { type: 'checkbox', checked: s.condition?.compare ? true : null }), cmpOff: inp(s.condition?.compare?.offset_s ?? 3600, { type: 'number' }), cmpMode: sel(['ratio', 'diff'], s.condition?.compare?.mode || 'ratio'),
    every: inp(s.schedule?.every_s ?? '', { type: 'number', placeholder: `${info.default_realtime_s} real-time / 60` }), cron: inp(s.schedule?.cron ?? '', { placeholder: 'or cron: */5 * * * *' }),
    look: inp(s.lookback_s, { type: 'number' }), cool: inp(s.cooldown_s, { type: 'number' }), budget: inp(s.budget_ms ?? '', { type: 'number', placeholder: 'ms' }),
    dfields: inp((s.dedup?.fields || []).join(', ')), dwin: inp(s.dedup?.window_s ?? 0, { type: 'number' }),
    subj: inp(s.template?.subject, { placeholder: info.default_subject }), body: h('textarea', { rows: 4, placeholder: info.default_body }, s.template?.body || ''),
    comp: h('textarea', { rows: 4, spellcheck: 'false' }, JSON.stringify(s.composite || { and: [{ alert: '' }] }, null, 2)),
    ak: inp(s.anomaly?.k ?? 3, { type: 'number', step: 'any' }), awin: inp(s.anomaly?.window ?? 30, { type: 'number' }), amin: inp(s.anomaly?.min_points ?? 10, { type: 'number' }), adir: sel(['both', 'high', 'low'], s.anomaly?.direction || 'both'),
    incident: h('input', { type: 'checkbox', checked: s.open_incident ? true : null }), enabled: h('input', { type: 'checkbox', checked: s.enabled !== false ? true : null }),
  };
  const destBoxes = dests.map((d) => [d, h('input', { type: 'checkbox', checked: (s.destinations || []).includes(d.id) ? true : null })]);
  const num = (el) => (el.value === '' ? undefined : Number(el.value));

  function collect() {
    const k = kind.value;
    const r = { name: els.name.value, kind: k, enabled: els.enabled.checked, description: els.desc.value, folder: els.folder.value, lookback_s: num(els.look), cooldown_s: num(els.cool) ?? 0, open_incident: els.incident.checked, destinations: destBoxes.filter(([, c]) => c.checked).map(([d]) => d.id), dedup: { fields: els.dfields.value.split(',').map((x) => x.trim()).filter(Boolean), window_s: num(els.dwin) ?? 0 }, template: { subject: els.subj.value, body: els.body.value } };
    if (k === 'composite') r.composite = JSON.parse(els.comp.value);
    else {
      r.query = qkind.value === 'llm'
        ? { kind: 'llm', llm: { metric: els.lmetric.value, ...(els.lmodel.value.trim() ? { model: els.lmodel.value.trim() } : {}), per_model: els.lper.checked } }
        : qkind.value === 'builder'
        ? { kind: 'builder', builder: { source: els.bsource.value, agg: els.bagg.value, field: els.bfield.value, filters: lines(els.bfilters.value).map((l) => ({ field: l.split('=')[0].trim(), value: l.slice(l.indexOf('=') + 1) })), group_by: els.bgroup.value.split(',').map((x) => x.trim()).filter(Boolean) } }
        : { kind: qkind.value, text: els.text.value };
      if (k === 'anomaly') r.anomaly = { k: num(els.ak), window: num(els.awin), min_points: num(els.amin), direction: els.adir.value };
      else r.condition = { op: els.op.value, threshold: num(els.thr) ?? 0, ...(els.cmpOn.checked ? { compare: { offset_s: num(els.cmpOff), mode: els.cmpMode.value } } : {}) };
    }
    r.schedule = els.cron.value ? { cron: els.cron.value } : els.every.value ? { every_s: num(els.every) } : {};
    if (k === 'realtime' && els.budget.value) r.budget_ms = num(els.budget);
    return r;
  }

  function paint() {
    const k = kind.value;
    mount(sections,
      k === 'composite'
        ? h('div', { class: 'card stack' }, h('h2', null, 'Expression'), h('p', { class: 'muted' }, 'AND / OR / NOT over other alerts: {"and": [{"alert": "<id>"}, {"not": {"alert": "<id>"}}]}. A loop is refused on save.'), els.comp)
        : h('div', { class: 'card stack' }, h('h2', null, 'Query'), field('qk', 'Language', qkind),
          qkind.value === 'llm'
            ? h('div', { class: 'stack' }, h('div', { class: 'row' }, field('lm', 'Metric', els.lmetric), field('lmo', 'Model', els.lmodel), h('label', { class: 'field' }, h('span', null, 'One alert per model'), els.lper)), h('span', { class: 'muted' }, 'Model calls over the look-back window, from the org\'s trace v2 table: cost in dollars (reported, plus the price table), tokens, calls, error rate in percent, p95 latency in ms. Dedup on the label model for one alert per model.'))
            : qkind.value === 'builder'
            ? h('div', { class: 'row' }, field('bs', 'Source', els.bsource), field('ba', 'Aggregate', els.bagg), field('bf', 'Column (not for count)', els.bfield), field('bfi', 'Filters', els.bfilters), field('bg', 'Group by', els.bgroup))
            : h('div', { class: 'stack' }, els.text, h('span', { class: 'muted' }, qkind.value === 'sql' ? 'One read-only statement. $__from and $__to are the look-back window. The last numeric column is the value; the other columns are labels (they decide the dedup fingerprint).' : 'PromQL evaluated now; each series is a label set.')),
          k === 'anomaly'
            ? h('div', { class: 'row' }, h('span', { class: 'muted' }, 'Return a time column first and the value last. Flags the newest point outside the median ± k·MAD of the points before it.'), field('k', 'k (MADs)', els.ak), field('w', 'Baseline points', els.awin), field('m', 'Needs at least', els.amin), field('d', 'Direction', els.adir))
            : h('div', { class: 'row' }, field('o', 'Fires when value', els.op), field('t', 'Threshold', els.thr), h('label', { class: 'field' }, h('span', null, 'Compare with the past'), els.cmpOn), field('co', 'Seconds back', els.cmpOff), field('cm', 'As', els.cmpMode))),
      h('div', { class: 'card row' }, h('h2', null, 'Schedule'), field('e', k === 'realtime' ? 'Every (s, 5-60)' : 'Every (s)', els.every), k === 'realtime' ? field('b', 'Cost budget (ms)', els.budget) : field('c', 'Cron (UTC)', els.cron), field('l', 'Look-back (s)', els.look), field('cd', 'Cooldown (s)', els.cool), field('df', 'Dedup fields', els.dfields), field('dw', 'Dedup window (s)', els.dwin)),
      h('div', { class: 'card stack' }, h('h2', null, 'Notify'),
        dests.length ? h('div', { class: 'row' }, destBoxes.map(([d, c]) => h('label', { class: 'row' }, c, `${d.name} (${d.spec.kind})`))) : h('span', { class: 'muted' }, ['No destinations yet: ', h('a', { href: href('alerts') }, 'add one under Alerts → Destinations')]),
        field('s', 'Subject', els.subj), field('bd', 'Message', els.body), h('span', { class: 'muted' }, `Variables: ${info.variables.map((v) => `{{${v}}}`).join(' ')} {{labels.<name>}}`),
        h('label', { class: 'row' }, els.incident, 'Open an incident when this fires')));
  }
  kind.onchange = paint;
  qkind.onchange = paint;

  const run = async (fn) => {
    try {
      await fn();
    } catch (e) {
      say(e.message, 'warn');
    }
  };
  mount(view,
    h('div', { class: 'page-head' }, h('h2', null, isNew ? 'New alert' : s.name), h('a', { href: href('alerts') }, '← Alerts')), msg,
    h('div', { class: 'card row' }, field('n', 'Name', els.name), field('f', 'Folder', els.folder), field('k', 'Kind', kind), field('d', 'Description', els.desc), h('label', { class: 'row' }, els.enabled, 'Enabled')),
    sections,
    h('div', { class: 'row' },
      h('button', { type: 'button', onclick: () => run(async () => { say('Running the rule once…'); const r = await api.get('alerts/preview', { rule: JSON.stringify(collect()) }); say(''); show(r.data); }) }, 'Preview (does not fire)'),
      h('button', { class: 'primary', type: 'button', onclick: () => run(async () => { const r = isNew ? await api.send('POST', 'alerts', collect()) : await api.send('PUT', `alerts/${id}`, collect()); say('Saved.', 'ok'); if (isNew) navigate('alerts', r.data.id); }) }, 'Save'),
      isNew ? null : h('button', { type: 'button', onclick: () => run(async () => { await api.send('POST', `alerts/${id}/incident`, {}); say('Incident opened.', 'ok'); }) }, 'Open incident'),
      isNew ? null : h('button', { type: 'button', class: 'ghost', onclick: () => confirm('Delete this alert?') && run(async () => { await api.send('DELETE', `alerts/${id}`); navigate('alerts'); }) }, 'Delete')),
    preview,
    isNew ? null : h('div', { class: 'card stack' }, h('h2', null, 'History'), h('div', { id: 'hist' })));

  function show(d) {
    mount(preview, h('div', { class: 'card stack' },
      h('div', { class: 'row' }, h('strong', null, 'Preview'), h('span', { class: `badge ${d.state === 'firing' ? 'bad' : 'ok'}` }, d.state), h('span', { class: 'muted' }, `${d.ms} ms · nothing was sent or saved`)),
      d.notes.map((n) => h('div', { class: 'callout' }, n)),
      dataTable({ columns: [{ key: 'labels', label: 'Labels', render: (r) => Object.entries(r.labels).map(([k, v]) => `${k}=${v}`).join(', ') || '(none)' }, { key: 'value', label: 'Value', num: true }, { key: 'compared', label: 'Compared', num: true }, { key: 'breached', label: 'Breached', render: (r) => (r.breached ? 'yes' : 'no') }], rows: d.samples, empty: 'The query returned no rows.' }),
      d.series.length ? h('div', { class: 'muted' }, d.series.map((x) => `${x.points.length} points, outliers at positions: ${x.flagged.join(', ') || 'none'}`).join(' · ')) : null,
      d.message ? h('pre', { class: 'sqlbox' }, `${d.message.subject}\n\n${d.message.body}${d.message.unknown_variables.length ? `\n\n(unknown variables: ${d.message.unknown_variables.join(', ')})` : ''}`) : null));
  }

  paint();
  if (!isNew) {
    const { data } = await api.get(`alerts/${id}/history`, { limit: 20 });
    const box = view.querySelector('#hist');
    mount(box, dataTable({ columns: [{ key: 'at_ms', label: 'When', render: (r) => new Date(r.at_ms).toLocaleString() }, { key: 'state', label: 'State' }, { key: 'outcome', label: 'Outcome' }], rows: data, empty: 'Not evaluated yet.' }));
  }
}
