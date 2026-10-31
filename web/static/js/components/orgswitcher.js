import { section, navigate } from '../router.js';
import { session, state } from '../state.js';
import { h } from '../util/dom.js';

/** Lists the orgs the caller may see (the server filters the list by membership). */
export function orgSwitcher() {
  const sel = h('select', { 'aria-label': 'Organization', title: 'Organization' }, h('option', { value: state.org }, state.org));
  const orgs = session.orgs.length ? session.orgs : [{ name: state.org }];
  sel.replaceChildren(...orgs.map((o) => h('option', { value: o.name, selected: o.name === state.org }, (o.branding && o.branding.name) || o.name)));
  sel.addEventListener('change', () => {
    state.org = sel.value;
    navigate(section());
  });
  return h('label', { class: 'row muted' }, 'Org', sel);
}
