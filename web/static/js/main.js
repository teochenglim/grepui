// Entry point: build the shell, register routes, start the router.

import * as api from './api.js';
import { buildShell } from './components/shell.js';
import { alertEditPage } from './pages/alert-edit.js';
import { alertsPage } from './pages/alerts.js';
import { enrichmentPage } from './pages/enrichment.js';
import { functionsPage } from './pages/functions.js';
import { pipelinesPage } from './pages/pipelines.js';
import { redactionPage } from './pages/redaction.js';
import { dashboardPage } from './pages/dashboard.js';
import { dashboardsPage } from './pages/dashboards.js';
import { catalogPage } from './pages/catalog.js';
import { connectPage } from './pages/connect.js';
import { healthPage } from './pages/health.js';
import { logsPage } from './pages/logs.js';
import { metricsPage } from './pages/metrics.js';
import { notFound } from './pages/notfound.js';
import { traceDetailPage } from './pages/trace-detail.js';
import { tracesPage } from './pages/traces.js';
import { serviceDetailPage } from './pages/service-detail.js';
import { serviceMapPage } from './pages/service-map.js';
import { servicesPage } from './pages/services.js';
import { playlistPage } from './pages/playlist.js';
import { publicPage } from './pages/public.js';
import { retentionPage } from './pages/retention.js';
import { rollupsPage } from './pages/rollups.js';
import { streamDetailPage } from './pages/stream-detail.js';
import { streamsPage } from './pages/streams.js';
import { loginPage } from './pages/login.js';
import { adminPage } from './pages/admin.js';
import { platformPage } from './pages/platform.js';
import { applyBranding } from './components/branding.js';
import { parseHash, route, start } from './router.js';
import { session, state } from './state.js';

route('admin', adminPage);
route('platform', platformPage);
route('logs', logsPage);
route('traces', tracesPage);
route('traces/:id', traceDetailPage);
route('dashboards', dashboardsPage);
route('dashboards/:id', dashboardPage);
route('playlists/:id', playlistPage);
route('public/:token', publicPage);
route('alerts', alertsPage);
route('pipelines', pipelinesPage);
route('functions', functionsPage);
route('enrichment', enrichmentPage);
route('redaction', redactionPage);
route('alerts/:id', alertEditPage);
route('metrics', metricsPage);
route('services', servicesPage);
route('services/:name', serviceDetailPage);
route('service-map', serviceMapPage);
route('catalog', catalogPage);
route('streams', streamsPage);
route('streams/:name', streamDetailPage);
route('connect', connectPage);
route('rollups', rollupsPage);
route('retention', retentionPage);
route('health', healthPage);
route('*', notFound);

async function boot() {
  const app = document.getElementById('app');
  const isPublic = parseHash().parts[1] === 'public';
  let me = null;
  try {
    me = (await api.global('auth/me')).data;
  } catch {
    me = null;
  }
  // A public dashboard link needs no sign-in; everything else does (unless --no-auth).
  if (me && !isPublic && !me.authenticated && !me.no_auth) {
    loginPage(app, me, () => location.reload());
    return;
  }
  session.me = me;
  if (me && (me.authenticated || me.no_auth)) {
    try {
      session.orgs = (await api.global('orgs')).orgs;
    } catch {
      session.orgs = [];
    }
    // Land in an org the person may use.
    const [org] = parseHash().parts;
    if (session.orgs.length && !session.orgs.some((o) => o.name === org) && !isPublic) {
      state.org = session.orgs[0].name;
      location.hash = `#/${state.org}/logs`;
    }
  }
  window.addEventListener('grepui:signin', () => location.reload(), { once: true });
  const shell = buildShell(app);
  applyBranding(state.org);
  start(shell.view, {
    beforeRender: (parts) => {
      document.body.classList.remove('tv');
      api.resetQueries();
      shell.paintNav(parts);
      applyBranding(state.org);
    },
  });
}

boot();
