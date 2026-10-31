// The catalog note of a service (R-V4): description, owner, tier and runbook link.

import * as api from '../api.js';
import { h } from '../util/dom.js';

const TIERS = ['', 'critical', 'high', 'medium', 'low'];
const hint = (e) => (/migrate/.test(e.message) ? 'The state store needs `grepui migrate` before notes can be saved.' : e.message);

/** A read-only summary of a note, or null when there is none. */
export function noteView(a) {
  if (!a) return null;
  return h('div', { class: 'note' },
    a.description ? h('p', null, a.description) : null,
    h('div', { class: 'row' },
      a.owner ? h('span', { class: 'chip-static' }, `owner: ${a.owner}`) : null,
      a.tier ? h('span', { class: `badge tier-${a.tier}` }, a.tier) : null,
      a.runbook ? h('a', { href: a.runbook, target: '_blank', rel: 'noopener noreferrer' }, 'Runbook') : null));
}

/** The edit form. onSaved(note|null) runs after a save or a clear. */
export function annotationForm(service, a, onSaved) {
  const desc = h('textarea', { rows: 3, maxlength: 500, 'aria-label': 'Description' }, a?.description || '');
  const owner = h('input', { type: 'text', maxlength: 100, value: a?.owner || '', 'aria-label': 'Owner' });
  const tier = h('select', { 'aria-label': 'Tier' }, TIERS.map((t) => h('option', { value: t, selected: t === (a?.tier || '') }, t || '(none)')));
  const runbook = h('input', { type: 'url', maxlength: 500, value: a?.runbook || '', placeholder: 'https://…', 'aria-label': 'Runbook link' });
  const msg = h('span', { class: 'muted' });
  const body = () => ({ description: desc.value, owner: owner.value, tier: tier.value, runbook: runbook.value });
  const run = async (fn) => {
    msg.textContent = 'Saving…';
    try {
      await fn();
    } catch (e) {
      msg.textContent = hint(e);
    }
  };
  return h('form', { class: 'noteform', onsubmit: (e) => (e.preventDefault(), run(async () => { const { data } = await api.send('PUT', `services/${encodeURIComponent(service)}/annotation`, body()); onSaved(data); })) },
    h('label', null, 'Description', desc), h('label', null, 'Owner', owner), h('label', null, 'Tier', tier), h('label', null, 'Runbook', runbook),
    h('div', { class: 'row' },
      h('button', { class: 'primary', type: 'submit' }, 'Save note'),
      h('button', { type: 'button', onclick: () => run(async () => { await api.send('DELETE', `services/${encodeURIComponent(service)}/annotation`); onSaved(null); }) }, 'Clear'),
      msg));
}
