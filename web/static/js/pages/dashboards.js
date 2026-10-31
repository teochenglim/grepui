// Dashboards: folders, dashboards, import and export, bulk move, and playlists.

import * as api from '../api.js';
import { stateBox } from '../components/table.js';
import { href } from '../router.js';
import { h, mount } from '../util/dom.js';
import { fmtAge } from '../util/format.js';

const hint = (e) => (/migrate/.test(e.message) ? 'The state store needs `grepui migrate` (schema version 3) before dashboards can be used.' : e.message);
const save = (name, obj) => h('a', { href: URL.createObjectURL(new Blob([JSON.stringify(obj, null, 2)], { type: 'application/json' })), download: name }).click();

export async function dashboardsPage(view) {
  const out = h('div', { class: 'stack' }, stateBox('Loading…'));
  const msg = h('div');
  const say = (t, kind = '') => mount(msg, t ? h('div', { class: `callout ${kind}` }, t) : null);
  const picked = new Set();
  mount(view, h('div', { class: 'page-head' }, h('h2', null, 'Dashboards'), h('span', { class: 'sub' }, 'folders, panels, variables and public links')), msg, out);

  const act = async (fn) => {
    try {
      await fn();
      await load();
    } catch (e) {
      say(hint(e), 'warn');
    }
  };

  async function load() {
    try {
      const [f, d, p] = await Promise.all([api.get('folders'), api.get('dashboards'), api.get('playlists')]);
      const folders = f.data, dashes = d.data, lists = p.data;
      const row = (x) => h('div', { class: 'row' },
        h('input', { type: 'checkbox', 'aria-label': `Select ${x.title}`, checked: picked.has(x.id) || null, onchange: (e) => (e.target.checked ? picked.add(x.id) : picked.delete(x.id)) }),
        h('a', { href: href('dashboards', x.id), class: 'grow' }, x.title), h('span', { class: 'muted' }, `updated ${fmtAge(x.updated_ms)}`));
      const move = h('select', { 'aria-label': 'Move selected to' }, h('option', { value: '' }, '(no folder)'), folders.map((x) => h('option', { value: x.id }, x.name)));
      const newDash = h('select', { 'aria-label': 'Folder for the new dashboard' }, h('option', { value: '' }, '(no folder)'), folders.map((x) => h('option', { value: x.id }, x.name)));
      const importer = h('textarea', { rows: 6, spellcheck: 'false', placeholder: 'Paste an exported dashboard (or a bundle) as JSON', 'aria-label': 'Import JSON' });
      const section = (title, items) => h('div', { class: 'card stack' }, title, items.length ? items.map(row) : h('span', { class: 'muted' }, 'Empty.'));
      mount(out,
        h('div', { class: 'row' },
          h('button', { class: 'primary', type: 'button', onclick: () => { const t = prompt('Dashboard title'); t && act(async () => { const r = await api.send('POST', 'dashboards', { title: t, folder_id: newDash.value }); location.hash = href('dashboards', r.data.id); }); } }, 'New dashboard'), newDash,
          h('button', { type: 'button', onclick: () => { const n = prompt('Folder name'); n && act(() => api.send('POST', 'folders', { name: n })); } }, 'New folder'),
          h('button', { type: 'button', onclick: async () => save('grepui-dashboards.json', (await api.get('dashboards/export')).data) }, 'Export all')),
        h('div', { class: 'row' }, h('span', { class: 'muted' }, 'Selected:'), move,
          h('button', { type: 'button', onclick: () => picked.size ? act(async () => { await api.send('POST', 'dashboards/move', { ids: [...picked], folder_id: move.value }); picked.clear(); say('Moved.', 'ok'); }) : say('Select dashboards first.') }, 'Move')),
        folders.map((fo) => h('div', null, h('div', { class: 'folder-head' }, h('h2', null, `📁 ${fo.name}`),
          h('button', { type: 'button', class: 'ghost', onclick: () => { const n = prompt('Rename folder', fo.name); n && act(() => api.send('PUT', `folders/${fo.id}`, { name: n })); } }, 'Rename'),
          h('button', { type: 'button', class: 'ghost', onclick: () => confirm(`Delete the folder "${fo.name}" and every dashboard in it?`) && act(async () => { const r = await api.send('DELETE', `folders/${fo.id}`); say(`Deleted the folder and ${r.data.dashboards_deleted} dashboard(s).`, 'ok'); }) }, 'Delete'),
          h('button', { type: 'button', class: 'ghost', onclick: async () => save(`${fo.name}.json`, (await api.get('dashboards/export', { folder: fo.id })).data) }, 'Export')),
          section('', dashes.filter((x) => x.folder_id === fo.id)))),
        section(folders.length ? 'No folder' : '', dashes.filter((x) => !folders.some((fo) => fo.id === x.folder_id))),
        h('div', { class: 'card stack' }, h('h2', null, 'Import'), importer,
          h('div', { class: 'row' }, h('button', { type: 'button', onclick: () => act(async () => { const res = await fetch(`/api/v1/orgs/${encodeURIComponent(location.hash.split('/')[1] || 'default')}/dashboards/import${move.value ? `?folder=${move.value}` : ''}`, { method: 'POST', body: importer.value, headers: { Accept: 'application/json' } }); const b = await res.json(); if (!res.ok) throw new Error(b.error); importer.value = ''; say(`Imported ${b.data.imported.length} dashboard(s).`, 'ok'); }) }, 'Import (into the folder chosen above)'))),
        h('div', { class: 'card stack' }, h('h2', null, 'Playlists'), h('p', { class: 'muted' }, 'A playlist shows its dashboards in turn on a wall display; every display shows the same one at the same moment.'),
          lists.map((l) => h('div', { class: 'row' }, h('a', { class: 'grow', href: href('playlists', l.id) }, `▶ ${l.title}`), h('button', { type: 'button', class: 'ghost', onclick: () => confirm(`Delete the playlist "${l.title}"?`) && act(() => api.send('DELETE', `playlists/${l.id}`)) }, 'Delete'))),
          h('div', { class: 'row' }, h('button', { type: 'button', onclick: () => { if (!picked.size) return say('Select the dashboards for the playlist first (in order of selection).'); const t = prompt('Playlist name'); const s = Number(prompt('Seconds per dashboard', '30')); t && act(async () => { await api.send('POST', 'playlists', { title: t, spec: { dashboards: [...picked], interval_s: s } }); picked.clear(); say('Playlist created.', 'ok'); }); } }, 'New playlist from selected'))));
    } catch (e) {
      mount(out, stateBox(hint(e), 'error'));
    }
  }
  load();
}
