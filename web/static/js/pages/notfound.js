import { href } from '../router.js';
import { h } from '../util/dom.js';

export const notFound = (view) => view.append(h('div', { class: 'state' }, 'Page not found. ', h('a', { href: href('streams') }, 'Go to Streams')));
