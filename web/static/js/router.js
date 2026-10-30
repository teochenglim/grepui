// Hash router: #/{org}/{section}/{name}?range=1h

import { RANGES, state } from './state.js';

const routes = [];
let outlet = null;
let hooks = { beforeRender: () => {}, afterRender: () => {} };

export function route(pattern, page) {
  routes.push({ parts: pattern.split('/').filter(Boolean), page });
}

export function parseHash(hash = location.hash) {
  const [path, query = ''] = hash.replace(/^#/, '').split('?');
  return { parts: path.split('/').filter(Boolean).map(decodeURIComponent), query: new URLSearchParams(query) };
}

function match(parts) {
  for (const r of routes) {
    if (r.parts.length !== parts.length) continue;
    const params = {};
    const ok = r.parts.every((p, i) => (p.startsWith(':') ? ((params[p.slice(1)] = parts[i]), true) : p === parts[i]));
    if (ok) return { page: r.page, params };
  }
  return null;
}

/** Build a link that keeps the current org and range. */
export function href(...parts) {
  const q = state.range === '1h' ? '' : `?range=${state.range}`;
  return `#/${[state.org, ...parts].map(encodeURIComponent).join('/')}${q}`;
}

export function navigate(...parts) {
  location.hash = href(...parts);
}

export function refresh() {
  render();
}

async function render() {
  const { parts, query } = parseHash();
  if (parts.length === 0) return navigate('streams');
  state.org = parts[0];
  const r = query.get('range');
  if (r && RANGES[r]) state.range = r;
  const found = match(parts.slice(1)) || match(['*']);
  hooks.beforeRender(parts);
  const view = outlet;
  view.replaceChildren();
  try {
    await found.page(view, found.params);
  } catch (e) {
    const err = document.createElement('div');
    err.className = 'state error';
    err.textContent = e.message || String(e);
    view.replaceChildren(err);
  }
  hooks.afterRender(parts);
}

export function start(el, h) {
  outlet = el;
  hooks = { ...hooks, ...h };
  addEventListener('hashchange', render);
  render();
}

export const section = () => parseHash().parts[1] || 'streams';
