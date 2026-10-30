-- Per-minute RED rollup for grepui. Not yet run against a live GreptimeDB: check on your version.
-- Set "RED rollup table" in grepui Settings to: grepui_red_1m
--
-- The Flow creates its sink table if it does not exist (and adds an update_at column).
-- Adjust opentelemetry_traces if you use x-greptime-trace-table-name.

CREATE FLOW IF NOT EXISTS grepui_red_1m_flow
SINK TO grepui_red_1m
COMMENT 'Per-minute RED rollup for grepui'
AS
SELECT
  date_bin(INTERVAL '1 minute', timestamp) AS time_window,
  service_name,
  count(*) AS requests,
  sum(CASE WHEN span_status_code = 'STATUS_CODE_ERROR' THEN 1 ELSE 0 END) AS errors,
  sum(duration_nano) AS sum_ns,
  max(duration_nano) AS max_ns
FROM opentelemetry_traces
WHERE span_kind IN ('SPAN_KIND_SERVER', 'SPAN_KIND_CONSUMER')
  AND service_name IS NOT NULL
GROUP BY time_window, service_name;

-- Optional: keep raw spans for a short time and the rollup for longer.
-- ALTER TABLE opentelemetry_traces SET 'ttl' = '7d';
-- ALTER TABLE grepui_red_1m SET 'ttl' = '180d';

-- Optional one-off backfill of history that existed before the Flow (adjust the time bounds):
-- INSERT INTO grepui_red_1m (time_window, service_name, requests, errors, sum_ns, max_ns)
-- SELECT date_bin(INTERVAL '1 minute', timestamp), service_name, count(*),
--        sum(CASE WHEN span_status_code = 'STATUS_CODE_ERROR' THEN 1 ELSE 0 END),
--        sum(duration_nano), max(duration_nano)
-- FROM opentelemetry_traces
-- WHERE span_kind IN ('SPAN_KIND_SERVER', 'SPAN_KIND_CONSUMER') AND service_name IS NOT NULL
--   AND timestamp < now() - INTERVAL '1 minute'
-- GROUP BY 1, 2;
