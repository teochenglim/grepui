// Entry point: build the shell, register routes, start the router.

import * as api from './api.js';
import { buildShell } from './components/shell.js';
import { catalogPage } from './pages/catalog.js';
import { connectPage } from './pages/connect.js';
import { healthPage } from './pages/health.js';
import { logsPage } from './pages/logs.js';
import { notFound } from './pages/notfound.js';
import { traceDetailPage } from './pages/trace-detail.js';
import { tracesPage } from './pages/traces.js';
import { serviceDetailPage } from './pages/service-detail.js';
import { serviceMapPage } from './pages/service-map.js';
import { servicesPage } from './pages/services.js';
import { streamDetailPage } from './pages/stream-detail.js';
import { streamsPage } from './pages/streams.js';
import { route, start } from './router.js';

const shell = buildShell(document.getElementById('app'));

route('logs', logsPage);
route('traces', tracesPage);
route('traces/:id', traceDetailPage);
route('services', servicesPage);
route('services/:name', serviceDetailPage);
route('service-map', serviceMapPage);
route('catalog', catalogPage);
route('streams', streamsPage);
route('streams/:name', streamDetailPage);
route('connect', connectPage);
route('health', healthPage);
route('*', notFound);

start(shell.view, {
  beforeRender: (parts) => {
    api.resetQueries();
    shell.paintNav(parts);
  },
});
