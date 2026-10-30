import * as api from '../api.js';
import { section, navigate } from '../router.js';
import { state } from '../state.js';
import { h } from '../util/dom.js';

/** Lists the orgs the caller may see (all of them until access control lands in v0.11.0). */
export function orgSwitcher() {
  const sel = h('select', { 'aria-label': 'Organization', title: 'Organization' }, h('option', { value: state.org }, state.org));
  api.global('orgs').then(({ orgs }) => {
    sel.replaceChildren(...orgs.map((o) => h('option', { value: o.name, selected: o.name === state.org }, o.name)));
  }).catch(() => {});
  sel.addEventListener('change', () => {
    state.org = sel.value;
    navigate(section());
  });
  return h('label', { class: 'row muted' }, 'Org', sel);
}
