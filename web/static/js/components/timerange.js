import { refresh } from '../router.js';
import { RANGES, state } from '../state.js';
import { h } from '../util/dom.js';

export function timeRange() {
  const sel = h(
    'select',
    { 'aria-label': 'Time range' },
    Object.entries(RANGES).map(([k, r]) => h('option', { value: k, selected: k === state.range }, r.label)),
  );
  sel.addEventListener('change', () => {
    state.range = sel.value;
    const parts = location.hash.replace(/^#/, '').split('?')[0];
    location.hash = `${parts}${state.range === '1h' ? '' : `?range=${state.range}`}`;
    refresh();
  });
  return sel;
}

