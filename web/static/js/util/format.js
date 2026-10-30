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
