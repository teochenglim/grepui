// Helpers shared by the logs components: level styling, condition editing, request params.

export const LEVELS = ['', 'ERROR', 'WARN', 'INFO', 'DEBUG'];

export const levelClass = (l) => ({ ERROR: 'bad', WARN: 'warn', INFO: 'ok', DEBUG: '' })[String(l || '').toUpperCase()] ?? '';

/** `key=value` condition text for the filter bar, quoting values that need it. */
export function cond(key, value, op = '=') {
  const v = String(value);
  return `${key}${op}${/[\s"'=~!]/.test(v) || v === '' ? `"${v.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"` : v}`;
}

/** Append a condition to the filter text unless it is already there. */
export function addCondition(q, text) {
  const parts = q.trim() ? [q.trim()] : [];
  if (!q.includes(text)) parts.push(text);
  return parts.join(' ');
}

/** Map a field (column or `column.attributeKey`) to the filter-bar key. */
export function filterKey(field) {
  if (field === 'severity_text') return 'level';
  if (field.startsWith('resource_attributes.')) return `resource.${field.slice('resource_attributes.'.length)}`;
  if (field.startsWith('log_attributes.')) return field.slice('log_attributes.'.length);
  return field;
}
