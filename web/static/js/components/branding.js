// Per-org branding (R-C6): name, logo and accent colour from the org's settings.

import { h, mount } from '../util/dom.js';
import { session } from '../state.js';

/** Apply the branding of `org` (from the org list) to the page and the brand block. */
export function applyBranding(org) {
  const o = session.orgs.find((x) => x.name === org);
  const b = (o && o.branding) || {};
  const root = document.documentElement.style;
  if (/^#[0-9a-fA-F]{6}$/.test(b.accent || '')) root.setProperty('--accent', b.accent);
  else root.removeProperty('--accent');
  document.title = b.name ? `${b.name} · grepui` : 'grepui';
  const el = document.getElementById('brand');
  if (!el) return;
  mount(el,
    b.logo_url && /^https?:\/\//.test(b.logo_url) ? h('img', { class: 'logo-img', src: b.logo_url, alt: '', referrerpolicy: 'no-referrer' }) : h('span', { class: 'logo' }, (b.name || 'g').slice(0, 1).toLowerCase()),
    b.name || 'grepui');
}
