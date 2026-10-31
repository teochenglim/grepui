// Sign-in and first-run setup (R-C1, R-C4): shown instead of the app until someone is signed in.

import { h, mount } from '../util/dom.js';

async function call(path, body) {
  const res = await fetch(`/api/v1/auth/${path}`, { method: 'POST', headers: { 'Content-Type': 'application/json', Accept: 'application/json' }, body: JSON.stringify(body) });
  const j = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(j.error || `HTTP ${res.status}`);
  return j;
}

export function loginPage(root, me, done) {
  const setup = me.setup_required;
  const msg = h('div');
  const user = h('input', { id: 'u', autocomplete: 'username', 'aria-label': 'Username', placeholder: 'username' });
  const pass = h('input', { id: 'p', type: 'password', autocomplete: setup ? 'new-password' : 'current-password', 'aria-label': 'Password', placeholder: setup ? 'password (10 characters or more)' : 'password' });
  const submit = async (e) => {
    e.preventDefault();
    try {
      await call(setup ? 'setup' : 'login', { username: user.value, password: pass.value });
      done();
    } catch (err) {
      mount(msg, h('div', { class: 'callout warn' }, err.message));
    }
  };
  mount(root, h('div', { class: 'login' }, h('form', { class: 'card stack', onsubmit: submit },
    h('div', { class: 'brand' }, h('span', { class: 'logo' }, 'g'), 'grepui'),
    h('h2', null, setup ? 'Create the first administrator' : 'Sign in'),
    setup ? h('p', { class: 'muted' }, 'This is a new installation. The person created here can manage every org and user.') : null,
    msg, user, pass,
    h('button', { class: 'primary', type: 'submit' }, setup ? 'Create and sign in' : 'Sign in'),
    me.oidc && !setup ? h('a', { class: 'btn', href: '/api/v1/auth/oidc/login' }, 'Sign in with single sign-on') : null)));
  user.focus();
}
