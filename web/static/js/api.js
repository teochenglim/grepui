// REST client for /api/v1. Every data response carries `meta` ("Show SQL"); we keep the
// ones from the current page so the drawer can list them.

import { state } from './state.js';

const metas = [];
const listeners = new Set();

export const queriesOnPage = () => metas.slice();
export const onQueries = (fn) => (listeners.add(fn), () => listeners.delete(fn));
export function resetQueries() {
  metas.length = 0;
  listeners.forEach((f) => f());
}

async function request(url, opts = {}) {
  let res;
  try {
    res = await fetch(url, { ...opts, headers: { Accept: 'application/json', ...(opts.headers || {}) } });
  } catch {
    throw new Error('Cannot reach grepui. Is the server running?');
  }
  const text = await res.text();
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    throw new Error(text.slice(0, 200) || res.statusText);
  }
  if (!res.ok) throw new Error(body.error || `HTTP ${res.status}`);
  return body;
}

/** GET /api/v1/orgs/{org}/{path}; returns { data, meta }. */
export async function get(path, params = {}) {
  const qs = new URLSearchParams(Object.entries(params).filter(([, v]) => v != null));
  const url = `/api/v1/orgs/${encodeURIComponent(state.org)}/${path}${qs.size ? `?${qs}` : ''}`;
  const body = await request(url);
  if (body.meta) {
    metas.push(body.meta);
    listeners.forEach((f) => f());
  }
  return body;
}

export const global = (path) => request(`/api/v1/${path}`);

/** POST/PUT/DELETE under /api/v1/orgs/{org}/: JSON in, { data, meta } out. */
export async function send(method, path, body) {
  const url = `/api/v1/orgs/${encodeURIComponent(state.org)}/${path}`;
  const res = await request(url, {
    method,
    headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (res.meta) {
    metas.push(res.meta);
    listeners.forEach((f) => f());
  }
  return res;
}
