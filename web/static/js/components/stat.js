import { h } from '../util/dom.js';

export const stat = (label, value, hint) =>
  h('div', { class: 'card stat' }, h('span', { class: 'label' }, label), h('span', { class: 'value' }, value), hint ? h('span', { class: 'hint' }, hint) : null);
