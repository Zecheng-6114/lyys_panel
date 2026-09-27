-- 1.2 指标保留策略：新增小时级聚合表。
-- 原始 5 秒采样只保留 7 天（metrics 表），超期数据按小时聚合（avg/max）
-- 落到本表长期保留（1 年），既控制库体积又不丢历史趋势。
-- hour_ts 为该小时起点对齐到整小时的 Unix 秒（UTC 整点；本地时区偏移为
-- 整小时时即本地整点，与前端展示一致），作主键天然去重。
CREATE TABLE IF NOT EXISTS metrics_hourly (
    hour_ts       INTEGER PRIMARY KEY,
    cpu_avg       REAL    NOT NULL,
    cpu_max       REAL    NOT NULL,
    mem_used_avg  INTEGER NOT NULL,
    mem_used_max  INTEGER NOT NULL,
    net_in_avg    INTEGER NOT NULL,
    net_in_max    INTEGER NOT NULL,
    net_out_avg   INTEGER NOT NULL,
    net_out_max   INTEGER NOT NULL
);
