// Draw one dashboard panel from its data response.

import { hbars } from '../charts/bars.js';
import { geoMap, heatmap, sankey } from '../charts/grid.js';
import { pie } from '../charts/pie.js';
import { gauge, stat } from '../charts/single.js';
import { xyChart } from '../charts/xy.js';
import { h } from '../util/dom.js';
import { fmtNum } from '../util/format.js';
import { dataTable } from './table.js';

/** Markdown and HTML come back already sanitised; they still go into a script-less sandboxed frame. */
function frame(html) {
  const f = h('iframe', { class: 'htmlframe', sandbox: '', referrerpolicy: 'no-referrer', title: 'Panel content' });
  f.srcdoc = `<!doctype html><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'"><base target="_blank"><style>body{font:14px system-ui,sans-serif;margin:8px;color:#222}a{color:#0a7}pre,code{background:#0001;border-radius:4px;padding:1px 4px}table{border-collapse:collapse}td,th{border:1px solid #8884;padding:3px 8px}</style>${html}`;
  return f;
}

export function renderPanel(data, opts = {}) {
  const { type, columns = [], rows = [] } = data;
  if (type === 'markdown' || type === 'html') return frame(data.html || '');
  if (data.shape_error) return h('div', { class: 'callout warn' }, data.shape_error);
  if (!rows.length && type !== 'table') return h('div', { class: 'empty' }, 'No data in this range.');
  switch (type) {
    case 'line': case 'area': case 'stacked_area': case 'scatter': case 'bar_v': case 'stacked_bar_v':
      return xyChart(type, columns, rows, opts, data.compare);
    case 'bar_h': case 'stacked_bar_h': return hbars(type, columns, rows, opts);
    case 'pie': case 'donut': return pie(type, columns, rows, opts);
    case 'heatmap': return heatmap(columns, rows, opts);
    case 'stat': return stat(columns, rows, opts);
    case 'gauge': return gauge(columns, rows, opts);
    case 'geo_map': return geoMap(columns, rows, opts);
    case 'sankey': return sankey(columns, rows, opts);
    default:
      return h('div', null, dataTable({ rows, columns: columns.map((key) => ({ key, num: typeof rows[0]?.[key] === 'number', render: (r) => (typeof r[key] === 'number' ? fmtNum(r[key]) : r[key] == null ? '' : String(r[key])) })), empty: 'No rows.' }), data.truncated ? h('p', { class: 'muted' }, 'Showing the first rows only.') : null);
  }
}
