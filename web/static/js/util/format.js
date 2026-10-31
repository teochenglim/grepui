// Number, time and size formatting shared by pages.

const nf = new Intl.NumberFormat();
export const fmtNum = (n) => (n == null || Number.isNaN(Number(n)) ? '–' : nf.format(Number(n)));

const pad = (n) => String(n).padStart(2, '0');
export function fmtTime(ms) {
  if (ms == null) return '–';
  const d = new Date(Number(ms));
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export function fmtAge(ms, now = Date.now()) {
  if (ms == null) return '–';
  const s = Math.max(0, Math.round((now - Number(ms)) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}

export function fmtCell(v) {
  if (v == null) return '';
  if (typeof v === 'object') return JSON.stringify(v);
  return String(v);
}

export function fmtBytes(n) {
  if (n == null || Number.isNaN(Number(n))) return '–';
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = Number(n), i = 0;
  while (v >= 1024 && i < u.length - 1) (v /= 1024), i++;
  return `${v >= 10 || i === 0 ? Math.round(v) : v.toFixed(1)} ${u[i]}`;
}

export function fmtDuration(secs) {
  if (secs == null) return '–';
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
  return `${Math.floor(secs / 86400)}d`;
}

/** Microseconds as a short duration: 850 µs, 12.4 ms, 1.82 s. */
export function fmtUs(us) {
  if (us == null || Number.isNaN(Number(us))) return '–';
  const v = Number(us);
  if (v < 1000) return `${Math.round(v)} µs`;
  if (v < 1e6) return `${(v / 1000).toFixed(v < 1e4 ? 2 : 1)} ms`;
  return `${(v / 1e6).toFixed(2)} s`;
}

/** A stable colour per service name. */
export function serviceColor(name) {
  let hash = 0;
  for (const c of String(name || '')) hash = (hash * 31 + c.charCodeAt(0)) >>> 0;
  return `hsl(${hash % 360} 55% 52%)`;
}
