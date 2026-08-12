# alertd

`alertd` 是一个轻量、配置驱动的 Linux 告警守护程序。每台机器运行一个实例，监控进程、POSIX SHM、journald、磁盘和内存；异常、持续、恢复和日报统一发到钉钉。

```text
Collector → Observation → Alarm Engine → Event → Durable Queue → DingTalk
```

项目刻意保持简单：一个 Rust 二进制、一份 TOML、一个 systemd unit；不依赖 Prometheus、数据库、中心服务或动态插件。

## 构建与检查

```sh
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
```

Linux 部署前先检查配置：

```sh
alertd --config /etc/alertd/alertd.toml --check-config
alertd --config /etc/alertd/alertd.toml --dry-run
alertd --config /etc/alertd/alertd.toml --send-test
```

完整配置见 `config/alertd.toml.example`。未知字段、重复 check 名称和非法范围会被拒绝。钉钉密钥只从配置指定的环境变量读取；`/etc/alertd/alertd.env` 必须为 root 所有且权限 `0600`。

`runtime.host` 表示机器的稳定角色名，`runtime.ip` 可选，表示值班人员用于识别或连接该实例的主 IP。IP 不自动探测，避免多网卡、NAT 或隧道环境选错地址。

修改 TOML 后向进程发送 `SIGHUP`。新配置会先被完整解析和校验；失败时继续运行旧配置。

## Check 类型

- `process`：扫描 `/proc/<pid>/cmdline`，检查匹配进程数量。
- `shm`：支持 `exists`、`u64_counter` 和 `gconf_v2`；可只检查存在，也可检查进度停滞。`gconf_v2` 必须显式配置 ABI 的 `magic` 与 `layout_version`，BcastRing 按 head、Board 按 header heartbeat 与尾部 slot seqlock 判断进度；未知 SegKind fail-closed。
- `journal`：按 systemd unit 读取 journald，使用普通子串规则区分 WARN/CRITICAL。
- `systemd`：通过 `systemctl show` 检查一组 service/timer 是否均为 loaded、active。
- `latest_file`：按目录、前后缀选择最新普通文件，检查最小大小和 mtime 新鲜度，适用于滚动 raw/因子文件。
- `disk`：按挂载点已用比例分级。
- `memory`：按 `MemAvailable/MemTotal` 分级。

所有 collector 只产出事实。统一告警状态机负责等待、升级、重复与恢复；采集器连续失败会产生独立的“监控采集盲区”告警。

## 可靠投递

告警发送前以临时文件、`fsync` 和原子 rename 写入 `/var/lib/alertd/spool`，成功投递后才删除。重启后继续投递，模糊失败可能产生重复，但不会静默丢失。损坏队列文件会移动到 `spool/quarantine` 并在本地记录 ERROR。

## 消息示例

```text
🔴 CRITICAL · 告警

主机：bybit-sg

IP：203.0.113.10

检查：bybit-book

状态：SHM 已 180 秒没有推进

异常开始：2026-08-12 14:31:20

对象：/shm_bybit_lin_book_tick_v2

进度：812945

处理：https://runbook.example/shm-stale
```

## 首次上线

先在非关键主机与旧 monitor 并行运行 24 小时，并使用测试钉钉群。确认异常、恢复、日报和网络中断补发后，再逐机切换；不要让两套 monitor 同时向正式群发送同一规则。
