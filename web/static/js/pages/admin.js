// Org administration (R-C3, R-C5, R-C6): members, API tokens, the audit trail and branding of one org.

import * as api from '../api.js';
import { dataTable, stateBox } from '../components/table.js';
import { applyBranding } from '../components/branding.js';
import { copyText, h, mount } from '../util/dom.js';
import { fmtTime } from '../util/format.js';
import { session, state } from '../state.js';

const TABS = [['members', 'Members'], ['tokens', 'API tokens'], ['queries', 'Queries'], ['groups', 'Work groups'], ['llm', 'LLM'], ['audit', 'Audit'], ['branding', 'Branding']];
const ROLES = ['viewer', 'editor', 'admin'];
const sel = (opts, v, label) => h('select', { 'aria-label': label }, opts.map((o) => h('option', { value: o, selected: o === v || null }, o)));

export async function adminPage(view, _p, tab = 'members') {
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  const msg = h('div');
  const say = (t, kind = '') => mount(msg, t ? h('div', { class: `callout ${kind}` }, t) : null);
  const act = async (fn) => { try { await fn(); await load(); } catch (e) { say(e.message, 'warn'); } };
  mount(view,
    h('div', { class: 'page-head' }, h('h2', null, 'Administration'), h('span', { class: 'sub' }, `org ${state.org}`)),
    h('div', { class: 'tabs', role: 'tablist' }, TABS.map(([k, l]) => h('button', { type: 'button', role: 'tab', class: 'tab', 'aria-selected': k === tab, onclick: () => adminPage(view, _p, k) }, l))), msg, out);

  const views = {
    async members() {
      const { data } = await api.get('members');
      const name = h('input', { placeholder: 'username', 'aria-label': 'Username' });
      const pw = h('input', { type: 'password', placeholder: 'password (new people only)', 'aria-label': 'Password', autocomplete: 'new-password' });
      const role = sel(ROLES, 'viewer', 'Role');
      mount(out,
        dataTable({
          columns: [
            { key: 'username', label: 'Person' }, { key: 'display', label: 'Name' }, { key: 'source', label: 'Account' },
            { key: 'role', label: 'Role', render: (m) => { const s = sel(ROLES, m.role, `Role of ${m.username}`); s.onchange = () => act(() => api.send('PUT', `members/${m.username}`, { role: s.value }).then(() => say('Role changed.', 'ok'))); return s; } },
            { key: 'x', label: '', render: (m) => h('button', { type: 'button', class: 'ghost', onclick: () => confirm(`Remove ${m.username} from this org?`) && act(() => api.send('DELETE', `members/${m.username}`)) }, 'Remove') },
          ],
          rows: data, empty: 'No members.',
        }),
        h('div', { class: 'card row' }, name, role, pw, h('button', { class: 'primary', type: 'button', onclick: () => act(async () => { await api.send('POST', 'members', { username: name.value, role: role.value, password: pw.value || undefined }); say('Member added.', 'ok'); }) }, 'Add member'),
          h('span', { class: 'muted' }, 'An existing person is added with just a username; a new one needs a password.')));
    },
    async tokens() {
      const { data } = await api.get('tokens');
      const name = h('input', { placeholder: 'name, e.g. ci-pipeline', 'aria-label': 'Token name' });
      const role = sel(ROLES, 'viewer', 'Token role');
      const days = h('input', { type: 'number', min: '1', max: '3650', placeholder: 'expires in days (optional)', 'aria-label': 'Expires in days', style: 'width:210px' });
      mount(out,
        dataTable({
          columns: [
            { key: 'name', label: 'Token' }, { key: 'role', label: 'Role' }, { key: 'created_by', label: 'Created by' },
            { key: 'created_ms', label: 'Created', render: (t) => fmtTime(t.created_ms) },
            { key: 'expires_ms', label: 'Expires', render: (t) => (t.expires_ms ? fmtTime(t.expires_ms) : 'never') },
            { key: 'x', label: '', render: (t) => h('button', { type: 'button', class: 'ghost', onclick: () => confirm(`Revoke "${t.name}"? Anything using it stops working.`) && act(() => api.send('DELETE', `tokens/${t.id}`)) }, 'Revoke') },
          ],
          rows: data, empty: 'No tokens.',
        }),
        h('div', { class: 'card row' }, name, role, days, h('button', { class: 'primary', type: 'button', onclick: async () => {
          try {
            const r = await api.send('POST', 'tokens', { name: name.value, role: role.value, expires_days: days.value ? Number(days.value) : undefined });
            const tok = r.data.token;
            mount(msg, h('div', { class: 'callout ok stack' }, h('strong', null, 'Copy this token now: it is shown only once.'), h('code', { class: 'sqlbox' }, tok), h('div', { class: 'row' }, h('button', { type: 'button', onclick: async () => (await copyText(tok)) && say('Copied. This is the only time it is shown.', 'ok') }, 'Copy'), h('span', { class: 'muted' }, 'Use it as: Authorization: Bearer <token>'))));
            await load(true);
          } catch (e) { say(e.message, 'warn'); }
        } }, 'Create token')));
    },
    async llm() {
      const [{ data: prices }, { data: content }] = await Promise.all([api.get('llm/prices'), api.get('llm/content')]);
      const model = h('input', { placeholder: 'model name, e.g. gpt-4o', 'aria-label': 'Model' });
      const prompt = h('input', { type: 'number', min: '0', step: 'any', placeholder: 'prompt $ per 1M tokens', 'aria-label': 'Prompt price per million tokens', style: 'width:200px' });
      const completion = h('input', { type: 'number', min: '0', step: 'any', placeholder: 'completion $ per 1M tokens', 'aria-label': 'Completion price per million tokens', style: 'width:220px' });
      const mode = sel(['masked', 'hidden', 'visible'], content.mode, 'Prompt and completion text for people below admin');
      mode.addEventListener('change', () => act(() => api.send('PUT', 'llm/content', { mode: mode.value })));
      mount(out,
        h('div', { class: 'card stack' }, h('h3', null, 'Prompts and completions'),
          h('p', { class: 'muted' }, 'What editors and viewers (and anyone on a public link) see of message text, tool arguments and document text. Admins always see it. masked shows the length only; hidden removes it; visible shows it to signed-in members (public links stay masked).'),
          h('div', { class: 'row' }, mode)),
        h('div', { class: 'card stack' }, h('h3', null, 'Model prices'),
          h('p', { class: 'muted' }, 'Used only for spans that report no cost of their own (llm.cost.total). Dollars per million tokens. Estimated costs are marked ≈.'),
          dataTable({ columns: [{ key: 'model', label: 'Model' }, { key: 'prompt_per_1m', label: 'Prompt $/1M', num: true }, { key: 'completion_per_1m', label: 'Completion $/1M', num: true },
            { key: 'x', label: '', render: (p) => h('button', { type: 'button', class: 'ghost', onclick: () => act(() => api.send('DELETE', `llm/prices?model=${encodeURIComponent(p.model)}`)) }, 'Remove') }], rows: prices, empty: 'No prices set.' }),
          h('div', { class: 'row' }, model, prompt, completion, h('button', { class: 'primary', type: 'button', onclick: () => act(() => api.send('PUT', 'llm/prices', { model: model.value, prompt_per_1m: Number(prompt.value), completion_per_1m: Number(completion.value) })) }, 'Save price'))));
    },
    async queries() {
      const { data } = await api.get('queries');
      const stats = await api.get('queries/stats').then((r) => r.data).catch(() => ({ slowest: [], most_frequent: [] }));
      const cols = [{ key: 'user', label: 'Person' }, { key: 'tag', label: 'Query' }, { key: 'count', label: 'Runs', num: true }, { key: 'avg_ms', label: 'Avg ms', num: true }, { key: 'max_ms', label: 'Max ms', num: true }, { key: 'errors', label: 'Errors', num: true }];
      mount(out,
        h('div', { class: 'card stack' }, h('h3', null, 'Running now'),
          dataTable({
            columns: [{ key: 'user', label: 'Person' }, { key: 'tag', label: 'Query' }, { key: 'elapsed_ms', label: 'Running ms', num: true },
              { key: 'greptime_id', label: 'GreptimeDB process', render: (r) => r.greptime_id || '–' },
              { key: 'x', label: '', render: (r) => h('button', { type: 'button', class: 'ghost', onclick: () => confirm(`Cancel ${r.tag} by ${r.user}?`) && act(async () => { const c = await api.send('POST', `queries/${r.id}/cancel`); say(c.data.killed ? 'Cancelled and stopped in GreptimeDB.' : 'Cancelled.', 'ok'); }) }, 'Cancel') }],
            rows: data.running, empty: 'Nothing is running.',
          }),
          h('button', { type: 'button', onclick: () => load(true) }, 'Refresh')),
        h('div', { class: 'card stack' }, h('h3', null, 'Slowest (since start)'), dataTable({ columns: cols, rows: stats.slowest, empty: 'No queries yet.' })),
        h('div', { class: 'card stack' }, h('h3', null, 'Most frequent (since start)'), dataTable({ columns: cols, rows: stats.most_frequent, empty: 'No queries yet.' })));
    },
    async groups() {
      const { data } = await api.get('workgroups');
      const num = (v) => h('input', { type: 'number', min: '0', value: String(v), style: 'width:110px' });
      mount(out, h('p', { class: 'muted' }, 'Limits per role, checked before a query runs. 0 means no limit. The error a person sees names the limit that stopped them.'),
        data.map((g) => {
          const f = { max_concurrent: num(g.limits.max_concurrent), timeout_secs: num(g.limits.timeout_secs), max_rows: num(g.limits.max_rows), max_window_hours: num(g.limits.max_window_hours) };
          const field = (k, l) => h('label', { class: 'field' }, h('span', null, l), f[k]);
          return h('div', { class: 'card row' }, h('strong', null, g.role),
            field('max_concurrent', 'Running at once'), field('timeout_secs', 'Timeout (s)'), field('max_rows', 'Max rows'), field('max_window_hours', 'Max window (h)'),
            h('button', { class: 'primary', type: 'button', onclick: () => act(async () => { await api.send('PUT', `workgroups/${g.role}`, Object.fromEntries(Object.entries(f).map(([k, el]) => [k, Number(el.value) || 0]))); say(`Saved limits for ${g.role}.`, 'ok'); }) }, 'Save'));
        }));
    },
    async audit() {
      const user = h('input', { placeholder: 'user', 'aria-label': 'User', style: 'width:140px' });
      const action = h('input', { placeholder: 'action, e.g. GET /api/v1/orgs/{org}/logs', 'aria-label': 'Action', style: 'width:300px' });
      const hours = sel(['1', '24', '168', '720'], '24', 'Hours back');
      const table = h('div');
      const run = async () => {
        try {
          const to = Date.now();
          const { data } = await api.get('audit', { user: user.value || undefined, action: action.value || undefined, from: to - Number(hours.value) * 3600e3, to: to + 1000, limit: 200 });
          mount(table, dataTable({
            columns: [
              { key: 'at_ms', label: 'When', render: (r) => fmtTime(r.at_ms) }, { key: 'username', label: 'Who' }, { key: 'action', label: 'What' },
              { key: 'target', label: 'Target' }, { key: 'rows', label: 'Rows', num: true },
              { key: 'outcome', label: 'Outcome', render: (r) => h('span', { class: `badge ${r.outcome === 'ok' ? 'ok' : r.outcome === 'denied' ? 'bad' : 'warn'}` }, `${r.outcome} ${r.status || ''}`) },
            ],
            rows: data, empty: 'No matching entries.',
          }));
        } catch (e) { say(e.message, 'warn'); }
      };
      mount(out, h('div', { class: 'card row' }, user, action, 'last', hours, 'hours', h('button', { class: 'primary', type: 'button', onclick: run }, 'Search')), table);
      await run();
    },
    async branding() {
      const { data } = await api.get('branding');
      const f = { name: h('input', { value: data.name || '', 'aria-label': 'Display name' }), logo: h('input', { value: data.logo_url || '', placeholder: 'https://… (optional)', 'aria-label': 'Logo URL', class: 'grow' }), accent: h('input', { type: 'color', value: data.accent || '#14b8a6', 'aria-label': 'Accent colour' }) };
      mount(out, h('div', { class: 'card stack' }, h('h2', null, 'Branding'), h('p', { class: 'muted' }, 'Shown to everyone in this org: the name and logo at the top left and the accent colour.'),
        h('div', { class: 'row' }, h('label', { class: 'field' }, h('span', null, 'Name'), f.name), h('label', { class: 'field grow' }, h('span', null, 'Logo URL (http or https)'), f.logo), h('label', { class: 'field' }, h('span', null, 'Accent'), f.accent)),
        h('div', { class: 'row' }, h('button', { class: 'primary', type: 'button', onclick: () => act(async () => {
          const r = await api.send('PUT', 'branding', { name: f.name.value, logo_url: f.logo.value, accent: f.accent.value });
          const o = session.orgs.find((x) => x.name === state.org);
          if (o) o.branding = r.data;
          applyBranding(state.org);
          say('Saved.', 'ok');
        }) }, 'Save'))));
    },
  };
  async function load(keepMsg) {
    try {
      if (!keepMsg) say('');
      await views[tab]();
    } catch (e) { mount(out, stateBox(e.message, 'error')); }
  }
  load();
}
