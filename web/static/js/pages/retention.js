// Retention (R-X1): the TTL of each stream, a preview of what a new TTL would expire, and the edit.

import * as api from '../api.js';
import { stateBox } from '../components/table.js';
import { h, mount } from '../util/dom.js';
import { fmtNum } from '../util/format.js';

export async function retentionPage(view) {
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  const msg = h('div');
  mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Retention'), h('span', { class: 'sub' }, 'how long each stream is kept')), msg, out);
  const say = (text, kind = '') => mount(msg, text ? h('div', { class: `callout ${kind}` }, text) : null);

  async function load() {
    try {
      const { data } = await api.get('retention');
      mount(out,
        data.admin_writes ? null : h('div', { class: 'callout warn' }, 'Changing retention needs admin writes: start grepui with --admin-writes (or GREPUI_ADMIN_WRITES=1).'),
        data.advice ? h('div', { class: 'callout' }, data.advice) : null,
        data.streams.map((s) => {
          const ttl = h('input', { type: 'text', value: s.ttl_secs ? '' : '', placeholder: '7d, 48h, 4w or forever', size: 22, 'aria-label': `New TTL for ${s.stream}` });
          const result = h('div', { class: 'muted' });
          const preview = async () => {
            try {
              const { data: p } = await api.get(`retention/${s.stream}/preview`, { ttl: ttl.value });
              mount(result, `With ${p.ttl}: ${fmtNum(p.would_expire)} of ${fmtNum(p.total)} rows are older than the cutoff and will be removed by compaction; ${fmtNum(p.would_keep)} stay.`);
              return true;
            } catch (e) {
              mount(result, h('span', { class: 'err' }, e.message));
              return false;
            }
          };
          const apply = async () => {
            if (!(await preview()) || !confirm(`Set the TTL of ${s.table} to ${ttl.value}? Rows older than that are deleted and cannot be restored.`)) return;
            try {
              await api.send('PUT', `retention/${s.stream}`, { ttl: ttl.value });
              say(`TTL of ${s.table} set to ${ttl.value}.`, 'ok');
              await load();
            } catch (e) {
              say(e.message, 'warn');
            }
          };
          return h('div', { class: 'card stack' },
            h('h2', null, `${s.stream} `, h('span', { class: 'muted mono' }, s.table)),
            s.exists
              ? h('div', { class: 'stack' },
                h('p', null, 'Current TTL: ', h('strong', null, s.ttl)),
                h('div', { class: 'row' }, ttl, h('button', { type: 'button', onclick: preview }, 'Preview'), h('button', { class: 'primary', type: 'button', disabled: !data.admin_writes, onclick: apply }, 'Apply')),
                result)
              : h('p', { class: 'muted' }, 'This table does not exist yet.'));
        }));
    } catch (e) {
      mount(out, stateBox(e.message, 'error'));
    }
  }
  load();
}
