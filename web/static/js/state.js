// App state: current org and time range. The URL hash is the source of truth for both.

export const RANGES = {
  '15m': { label: 'Last 15 minutes', ms: 15 * 60e3 },
  '1h': { label: 'Last hour', ms: 3600e3 },
  '6h': { label: 'Last 6 hours', ms: 6 * 3600e3 },
  '24h': { label: 'Last 24 hours', ms: 24 * 3600e3 },
  '7d': { label: 'Last 7 days', ms: 7 * 86400e3 },
};

export const state = { org: 'default', range: '1h' };

/** Who is signed in and which orgs they may see (filled at start-up by main.js). */
export const session = { me: null, orgs: [] };

/** The caller's role in an org: admin, editor, viewer or null. */
export function roleIn(org) {
  const me = session.me;
  if (!me) return null;
  if (me.platform_admin || me.no_auth) return 'admin';
  return me.orgs[org] || null;
}

/** The window every data call uses, resolved at call time so "last hour" stays current. */
export function timeWindow() {
  const to = Date.now();
  return { from: to - RANGES[state.range].ms, to };
}
