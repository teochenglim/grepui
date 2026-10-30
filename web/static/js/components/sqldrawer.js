// "Show SQL" (R-Q4): the statements the current page ran, with timing and bound values.

import * as api from '../api.js';
import { copybox } from './copybox.js';
import { h, mount } from '../util/dom.js';
import { fmtNum } from '../util/format.js';

export function sqlDrawer() {
  const body = h('div', { class: 'drawer-body' });
  const close = h('button', { class: 'ghost', type: 'button', 'aria-label': 'Close' }, 'Close');
  const drawer = h('aside', { class: 'drawer', 'aria-label': 'SQL for this page' }, h('div', { class: 'drawer-head' }, h('h2', null, 'SQL on this page'), close), body);
  const toggle = h('button', { type: 'button' }, 'Show SQL');

  const paint = () => {
    const qs = api.queriesOnPage();
    toggle.textContent = qs.length ? `Show SQL (${qs.length})` : 'Show SQL';
    mount(
      body,
      qs.length
        ? qs.map((m) =>
            h(
              'div',
              { class: 'q-item' },
              h('div', { class: 'q-meta' }, h('strong', null, m.query), h('span', { class: 'badge' }, `${fmtNum(m.ms)} ms`), h('span', { class: 'badge' }, `${fmtNum(m.rows)} rows`), h('span', { class: `badge ${m.source === 'raw' ? 'ok' : 'warn'}` }, m.source)),
              m.sql ? copybox(m.sql) : h('p', { class: 'muted' }, 'Derived from configuration; no SQL ran.'),
              m.binds?.length ? h('p', { class: 'muted mono' }, `? = ${m.binds.map((b) => JSON.stringify(b)).join(', ')}`) : null,
            ),
          )
        : [h('p', { class: 'muted' }, 'No queries yet on this page.')],
    );
  };
  api.onQueries(paint);
  toggle.addEventListener('click', () => drawer.classList.toggle('open'));
  close.addEventListener('click', () => drawer.classList.remove('open'));
  addEventListener('keydown', (e) => e.key === 'Escape' && drawer.classList.remove('open'));
  paint();
  return { toggle, drawer };
}
