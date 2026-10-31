// Turn a panel's tabular result into the series each chart draws.

const isNum = (v) => typeof v === 'number';

/** Column kinds: 'N' when every non-null value is a number, else 'T'. */
export function colKinds(columns, rows) {
  return columns.map((c) => {
    const vals = rows.map((r) => r[c]).filter((v) => v != null);
    return vals.length && vals.every(isNum) ? 'N' : 'T';
  });
}

/**
 * Series for line-like charts. Wide: first column x, every other numeric column a series.
 * Long: x, series name, value. Category: text label then numbers (x is the label, categorical).
 * Returns { categorical, series: [{ name, points: [[x, y]] }] }.
 */
export function toSeries(columns, rows) {
  const kinds = colKinds(columns, rows);
  if (kinds.length === 3 && kinds[0] === 'N' && kinds[1] === 'T' && kinds[2] === 'N') {
    const by = new Map();
    for (const r of rows) {
      const k = r[columns[1]] ?? '';
      if (!by.has(k)) by.set(k, []);
      by.get(k).push([r[columns[0]], r[columns[2]]]);
    }
    return { categorical: false, series: [...by].map(([name, points]) => ({ name, points: points.sort((a, b) => a[0] - b[0]) })) };
  }
  if (kinds[0] === 'T') {
    return { categorical: true, series: columns.slice(1).map((c) => ({ name: c, points: rows.map((r) => [r[columns[0]], r[c]]).filter((p) => p[1] != null) })) };
  }
  return { categorical: false, series: columns.slice(1).map((c) => ({ name: c, points: rows.map((r) => [r[columns[0]], r[c]]).filter((p) => p[1] != null).sort((a, b) => a[0] - b[0]) })) };
}

/** [label, value] pairs from a category result (first numeric column). */
export function toPairs(columns, rows) {
  const kinds = colKinds(columns, rows);
  const li = kinds.indexOf('T');
  const vi = kinds.indexOf('N');
  if (vi < 0) return [];
  return rows.map((r, i) => [li >= 0 ? String(r[columns[li]]) : String(i + 1), r[columns[vi]]]).filter((p) => p[1] != null);
}
