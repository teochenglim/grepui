-- Per-minute RED rollup, the same DDL grepui's Rollups page runs (verified on GreptimeDB v1.3.0).
-- Replace `public` and opentelemetry_traces if your database or trace table differ; the sink is
-- named <traces table>_red_1m and the flow grepui_<database>_red_1m.

CREATE TABLE IF NOT EXISTS `public`.`opentelemetry_traces_red_1m` (
  `service_name` STRING,
  `time_window` TIMESTAMP(3) NOT NULL,
  `requests` BIGINT,
  `errors` BIGINT,
  `sum_ns` BIGINT,
  `max_ns` BIGINT,
  TIME INDEX (`time_window`),
  PRIMARY KEY (`service_name`)
);

CREATE FLOW IF NOT EXISTS `grepui_public_red_1m`
SINK TO `public`.`opentelemetry_traces_red_1m`
AS
SELECT
  `service_name`,
  date_bin('1 minute'::INTERVAL, `timestamp`) AS `time_window`,
  count(*) AS `requests`,
  sum(CASE WHEN `span_status_code` = 'STATUS_CODE_ERROR' THEN 1 ELSE 0 END) AS `errors`,
  sum((CAST(`timestamp_end` AS BIGINT) - CAST(`timestamp` AS BIGINT))) AS `sum_ns`,
  max((CAST(`timestamp_end` AS BIGINT) - CAST(`timestamp` AS BIGINT))) AS `max_ns`
FROM `public`.`opentelemetry_traces`
WHERE `span_kind` IN ('SPAN_KIND_SERVER', 'SPAN_KIND_CONSUMER') AND `service_name` IS NOT NULL
GROUP BY `service_name`, `time_window`;

-- Backfill history written before the flow (whole minutes; run per day for large tables):
-- INSERT INTO `public`.`opentelemetry_traces_red_1m` (`service_name`, `time_window`, `requests`, `errors`, `sum_ns`, `max_ns`)
-- SELECT ... the same SELECT as above ... AND `timestamp` >= '2026-10-01 00:00:00' AND `timestamp` < '2026-10-02 00:00:00'
-- GROUP BY `service_name`, `time_window`;

-- Keep raw spans short and the rollup long:
-- ALTER TABLE `public`.`opentelemetry_traces` SET 'ttl' = '7d';
-- ALTER TABLE `public`.`opentelemetry_traces_red_1m` SET 'ttl' = '180d';
