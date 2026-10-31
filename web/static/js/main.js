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
import { route, start } from './router.js';

const shell = buildShell(document.getElementById('app'));

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

start(shell.view, {
  beforeRender: (parts) => {
    document.body.classList.remove('tv');
    api.resetQueries();
    shell.paintNav(parts);
  },
});
