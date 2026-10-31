// A playlist on a wall display: shows each dashboard in turn. The rotation comes from the
// server's clock, so several displays stay in step.

import * as api from '../api.js';
import { dashboardView } from '../components/dashview.js';
import { stateBox } from '../components/table.js';
import { href } from '../router.js';
import { RANGES, state } from '../state.js';
import { h, mount } from '../util/dom.js';

export async function playlistPage(view, { id }) {
  document.body.classList.add('tv');
  const holder = h('div');
  const title = h('h2');
  mount(view, h('div', { class: 'page-head' }, title, h('a', { href: href('dashboards') }, 'Exit')), holder);
  let shown = null;
  let timer = null;
  let current = null;
  const tick = async () => {
    if (!view.isConnected) return current?.stop();
    try {
      const { data: now } = await api.get(`playlists/${id}/now`);
      title.textContent = `${now.title} · ${now.index + 1}/${now.dashboards.length}`;
      if (now.dashboard_id !== shown) {
        shown = now.dashboard_id;
        const { data: dash } = await api.get(`dashboards/${shown}`);
        if (RANGES[dash.spec.time.range]) state.range = dash.spec.time.range;
        current?.stop();
        current = dashboardView(dash.spec, {
          data: (panel, qs) => api.get(`dashboards/${shown}/panels/${encodeURIComponent(panel)}/data?${qs}`),
          variables: (qs) => api.get(`dashboards/${shown}/variables?${qs}`),
        });
        mount(holder, h('h3', null, dash.title), current.el);
      }
      timer = setTimeout(tick, Math.max(1000, now.next_in_ms));
    } catch (e) {
      mount(holder, stateBox(e.message, 'error'));
      timer = setTimeout(tick, 15000);
    }
  };
  addEventListener('hashchange', () => clearTimeout(timer), { once: true });
  tick();
}
