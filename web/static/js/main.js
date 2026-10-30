// Entry point: build the shell, register routes, start the router.

import * as api from './api.js';
import { buildShell } from './components/shell.js';
import { connectPage } from './pages/connect.js';
import { notFound } from './pages/notfound.js';
import { streamDetailPage } from './pages/stream-detail.js';
import { streamsPage } from './pages/streams.js';
import { route, start } from './router.js';

const shell = buildShell(document.getElementById('app'));

route('streams', streamsPage);
route('streams/:name', streamDetailPage);
route('connect', connectPage);
route('*', notFound);

start(shell.view, {
  beforeRender: (parts) => {
    api.resetQueries();
    shell.paintNav(parts);
  },
});
