-- 监控维度补全：磁盘 I/O 速率入库。
-- monitor.rs 采集的 Snapshot 早已含 disk_read_per_sec / disk_write_per_sec
-- （由 /proc/diskstats 扇区计数差值换算），但落库时被丢掉了 —— 于是趋势图
-- 只能画 CPU% 与内存%，磁盘 I/O 没有历史可回溯。
--
-- 两列刻意**允许 NULL**：迁移之前的历史行无法重建（当时根本没落库），
-- 用 0 填充会把「没有数据」伪装成「当时磁盘 I/O 为零」。前端对 NULL 断开折线，
-- 视觉上正好表达「这段时间没有采集」。
ALTER TABLE metrics ADD COLUMN disk_read  INTEGER;
ALTER TABLE metrics ADD COLUMN disk_write INTEGER;

-- 小时聚合表同步补列（avg/max 各一对，与 net_in/net_out 同口径）。
-- 同样允许 NULL：老小时桶没有磁盘数据。
-- 注意：本迁移只负责加列。已有原始行的回填在 Rust 侧（Db::backfill_disk_hourly）
-- 运行时执行，不在迁移里 —— 回填要按小时分组聚合、量大且可重复执行，
-- 与「一次性改结构」混在一起会让失败时无法单独重试。
ALTER TABLE metrics_hourly ADD COLUMN disk_read_avg  INTEGER;
ALTER TABLE metrics_hourly ADD COLUMN disk_read_max  INTEGER;
ALTER TABLE metrics_hourly ADD COLUMN disk_write_avg INTEGER;
ALTER TABLE metrics_hourly ADD COLUMN disk_write_max INTEGER;
