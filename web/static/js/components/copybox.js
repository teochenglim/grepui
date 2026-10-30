import { copyText, h } from '../util/dom.js';

export function copybox(text) {
  const btn = h('button', { class: 'ghost', type: 'button' }, 'Copy');
  btn.addEventListener('click', async () => {
    btn.textContent = (await copyText(text)) ? 'Copied' : 'Select and copy';
    setTimeout(() => (btn.textContent = 'Copy'), 1500);
  });
  return h('div', { class: 'copybox' }, btn, h('pre', null, text));
}
