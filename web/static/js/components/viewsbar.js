// Saved views and recent searches (R-S2, R-S3). The URL is the state: applying a spec rewrites
// the query string and re-renders, so every applied view is also a shareable link.

import * as api from '../api.js';
import { href, refresh, replaceQuery } from '../router.js';
import { h, mount, copyText } from '../util/dom.js';
import { fmtAge } from '../util/format.js';

const KEYS = ['q', 'level', 'mode', 'sql'];

/** Apply a saved spec: set the URL parameters and re-render the page. */
export function applySpec(spec, viewId) {
  const patch = { view: viewId || null };
  for (const k of KEYS) patch[k] = spec[k] || null;
  patch.range = spec.range && spec.range !== '1h' ? spec.range : null;
  replaceQuery(patch);
  refresh();
}

const label = (spec) => (spec.mode === 'sql' ? spec.sql : [spec.level && `level=${spec.level}`, spec.q].filter(Boolean).join(' ')) || '(empty search)';
const hint = (e) => (/migrate/.test(e.message) ? 'The state store is not initialised. Run `grepui migrate` (the playground does it for you).' : e.message);

export function viewsBar({ kind, getSpec }) {
  const views = h('div', { class: 'menu-body' });
  const recent = h('div', { class: 'menu-body' });
  let loadedViews = false;
  let loadedRecent = false;

  async function paintViews() {
    mount(views, h('span', { class: 'muted' }, 'Loading…'));
    try {
      const { data } = await api.get('views', { kind });
      loadedViews = true;
      mount(
        views,
        data.length
          ? data.map((v) =>
              h('div', { class: 'menu-row' },
                h('a', { href: href('logs'), class: 'grow', onclick: (e) => (e.preventDefault(), applySpec(v.spec, v.id)) }, v.name),
                h('button', { class: 'ghost icon', type: 'button', title: 'Copy link', 'aria-label': `Copy link to ${v.name}`, onclick: async () => copyText(`${location.origin}${location.pathname}${href('logs').split('?')[0]}?view=${v.id}`) }, '🔗'),
                h('button', { class: 'ghost icon', type: 'button', title: 'Rename', 'aria-label': `Rename ${v.name}`, onclick: () => rename(v) }, '✎'),
                h('button', { class: 'ghost icon', type: 'button', title: 'Delete', 'aria-label': `Delete ${v.name}`, onclick: () => remove(v) }, '✕')))
          : h('span', { class: 'muted' }, 'No saved views yet.'),
        h('button', { class: 'primary', type: 'button', onclick: save }, 'Save current search…'),
      );
    } catch (e) {
      mount(views, h('span', { class: 'err' }, hint(e)));
    }
  }

  async function save() {
    const name = prompt('Name for this view');
    if (!name || !name.trim()) return;
    try {
      const { data } = await api.send('POST', 'views', { name: name.trim(), kind, spec: getSpec() });
      replaceQuery({ view: data.id });
      await paintViews();
    } catch (e) {
      alert(hint(e));
    }
  }
  async function rename(v) {
    const name = prompt('Rename view', v.name);
    if (!name || !name.trim() || name === v.name) return;
    try {
      await api.send('PUT', `views/${v.id}`, { name: name.trim() });
      await paintViews();
    } catch (e) {
      alert(hint(e));
    }
  }
  async function remove(v) {
    if (!confirm(`Delete the view "${v.name}"?`)) return;
    try {
      await api.send('DELETE', `views/${v.id}`);
      await paintViews();
    } catch (e) {
      alert(hint(e));
    }
  }

  async function paintRecent() {
    mount(recent, h('span', { class: 'muted' }, 'Loading…'));
    try {
      const { data } = await api.get('history', { kind, limit: 25 });
      loadedRecent = true;
      mount(recent, data.length
        ? data.map((e) => h('div', { class: 'menu-row' }, h('a', { href: href('logs'), class: 'grow mono clip', title: label(e.spec), onclick: (ev) => (ev.preventDefault(), applySpec(e.spec)) }, label(e.spec)), h('span', { class: 'muted nowrap' }, fmtAge(e.at_ms))))
        : h('span', { class: 'muted' }, 'Nothing yet. Searches you run appear here.'));
    } catch (e) {
      mount(recent, h('span', { class: 'err' }, hint(e)));
    }
  }

  const menu = (title, body, onOpen) => {
    const d = h('details', { class: 'menu' }, h('summary', null, title), body);
    d.addEventListener('toggle', () => d.open && onOpen());
    // Close when clicking elsewhere.
    document.addEventListener('click', (e) => d.open && !d.contains(e.target) && (d.open = false));
    return d;
  };
  return h('div', { class: 'row' },
    menu('Views', views, () => !loadedViews && paintViews()),
    menu('Recent', recent, paintRecent));
}

/** Record a search in the history (fire and forget: it must never get in the way). */
export function recordSearch(kind, spec) {
  if (!Object.values(spec).some(Boolean)) return;
  api.send('POST', 'history', { kind, spec }).catch(() => {});
}
