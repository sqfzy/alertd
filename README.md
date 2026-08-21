# alertd

`alertd` 是一个轻量、配置驱动的 Linux 告警守护程序。每台机器运行一个实例，监控应用链路、主机资源、校时、网络和低延迟运行态；异常、持续、恢复、事件和日报统一发到钉钉。

```text
Collector → Observation → Alarm Engine → Event → Durable Queue → DingTalk
```

项目刻意保持简单：一个 Rust 二进制、一份 TOML、一个 systemd unit；不依赖 Prometheus、数据库、中心服务或动态插件。

## 源码组织

```text
alertd/
├── src/
│   ├── main.rs             CLI 解析与进程入口
│   ├── config.rs           严格 TOML 配置模型与校验
│   ├── runtime.rs          采集循环、热加载、日报与自监控编排
│   ├── maintenance.rs      持久化维护窗口的 CLI 状态文件与生命周期
│   ├── alarm.rs            pending、重复、升级和恢复状态机
│   ├── model.rs            Observation、AlertEvent 与持久状态 POD
│   ├── report.rs           钉钉告警、内部事件和日报排版
│   ├── collectors/         各 check 的只读事实采集器
│   └── delivery/           持久队列与钉钉投递
├── config/                 完整配置示例与部署角色样例
├── deploy/                 systemd、密钥环境文件和测试部署材料
└── tests/                  配置、collector 与报告集成测试
```

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

所有外发消息还会携带实际系统 hostname 和发送实例指纹：`machine`、`boot`、`pid`、`config`。`machine` 用于区分复制了相同角色配置的机器，`boot` 用于区分同一机器的不同启动周期，`pid` 用于区分并行进程，`config` 用于识别实际生效的配置文件。`machine`、`boot` 和 `config` 的哈希来源分别是去除结尾空白后的 `/etc/machine-id`、`/proc/sys/kernel/random/boot_id` 和原始 TOML 字节；消息显示 SHA-256 前 12 位，启动日志记录完整 SHA-256。身份在消息进入持久队列时冻结，重启后补发也不会被改写成新进程身份。

可在目标机核对指纹：

```sh
printf %s "$(cat /etc/machine-id)" | sha256sum
printf %s "$(cat /proc/sys/kernel/random/boot_id)" | sha256sum
sha256sum /etc/alertd/alertd.toml
```

运行模式要求 Linux 身份文件存在且非空，否则 alertd 拒绝启动；`--check-config` 只检查 TOML，不依赖主机身份文件。

修改 TOML 后向进程发送 `SIGHUP`。新配置会先被完整解析和校验；失败时继续运行旧配置。

## 维护窗口

业务维护前可以立即暂停所有 check 告警，并指定自动恢复时间。该操作不修改 TOML，也不需要停止或重启 alertd：

```sh
alertd --config /etc/alertd/alertd.toml \
  maintenance start \
  --until '2026-08-21T16:00:00+08:00' \
  --reason '业务重新部署'

alertd --config /etc/alertd/alertd.toml maintenance status
alertd --config /etc/alertd/alertd.toml maintenance cancel
```

`--until` 必须是带时区的 RFC3339 绝对时间，并且至少晚于当前时间一分钟；不限制最长窗口。`--reason` 必须为 1–256 字节且不能包含控制字符。重复 `start` 会被拒绝，`cancel` 在没有窗口时也会成功返回。

CLI 将窗口原子写入 `runtime.state_dir/maintenance.json`，alertd 最多在一个采样周期内确认并向钉钉发送开始通知。正确的操作顺序是：**先设置维护窗口并用 `status` 确认，再停止业务服务**。`status` 会区分等待 daemon 确认、维护中、已取消或过期但等待结束通知，以及当前无窗口。

维护期间 collector、journal cursor、CPU/network/SHM 采样基线、watchdog、自监控、状态保存和投递 worker 继续工作；已有队列消息继续发送。新的 check 告警、采集盲区告警和日报不会产生，日志也不会在恢复后回放。维护前已有的 pending/firing 状态原样保留。到期或人工取消后，检查立即恢复，不等待结束通知实际送达；结束通知成功进入持久队列后窗口文件才会删除。维护期间跳过的当日日报会在恢复后的下一轮补发。

若窗口文件无法读取、JSON 损坏或字段非法，alertd 会 fail-open：保持正常监控，并发送一条去重的内部 WARN，避免错误状态造成长期静默。

## Check 类型

- `process`：扫描 `/proc/<pid>/cmdline`，检查匹配进程数量。
- `shm`：支持 `exists`、`u64_counter` 和 `gconf_v2`；可只检查存在，也可检查进度停滞。`gconf_v2` 必须显式配置 ABI 的 `magic` 与 `layout_version`，BcastRing 按 head、Board 按 header heartbeat 与尾部 slot seqlock 判断进度；未知 SegKind fail-closed。
- `journal`：按 systemd unit 读取 journald，使用普通、区分大小写的子串规则过滤已知噪声并区分 WARN/CRITICAL；`ignore_contains` 优先于告警规则。
- `systemd`：通过 `systemctl show` 检查一组 service/timer 是否均为 loaded、active。
- `latest_file`：按目录、前后缀选择最新普通文件，检查最小大小和 mtime 新鲜度，适用于滚动 raw/因子文件。
- `disk`：按挂载点容量和 inode 已用比例分级，取更高严重度。
- `memory`：按 `MemAvailable/MemTotal` 分级。
- `cpu`：按 `/proc/stat` 连续采样计算每个逻辑 CPU 的使用率，告警和日报均显示全部核心。
- `time_sync`：使用 `chronyc -c tracking` 检查同步状态和剩余时钟偏差。
- `network`：检查指定接口链路状态，以及 RX/TX error/drop 每秒速率。
- `system_tuning`：只读检查 `lat_tune.sh` 定义的当前内核、RT、irqbalance、IRQ、XPS/RPS 低延迟基线，不执行调优。

所有 collector 只产出事实。统一告警状态机负责等待、升级、重复与恢复防抖；采集器连续失败会产生独立的“监控采集盲区”告警。journald 是事件型检查：命中会聚合和限频，不会因下一轮没有新日志而发送虚假恢复。

`system_tuning` 严格检查当前运行态：CPU0 housekeeping、其余 present CPU 隔离，`isolcpus/nohz_full/rcu_nocbs`、`rcu_nocb_poll`、`irqaffinity=0`、`mitigations=off`、`nowatchdog`、`nosoftlockup`、RT throttle、irqbalance 和数据口 IRQ/XPS/RPS。基线来自 `lat_tune.sh` SHA-256 `27c6096d9b907b8207a5d440cce9c6c6ffce63d90a27ea37fc53870261377da8`。它不检查持久化文件，也不自动修复；`mitigations=off` 是以安全缓解换取延迟，只适用于受控隔离环境。

## 可靠投递

告警发送前以临时文件、`fsync` 和原子 rename 写入 `/var/lib/alertd/spool`，成功投递后才删除。重启后继续投递，模糊失败可能产生重复，但不会静默丢失。损坏队列文件会移动到 `spool/quarantine` 并在本地记录 ERROR。

业务告警最多使用队列的 `capacity - 1` 个槽位，最后一个槽保留给 alertd 自监控。配置热加载失败、状态保存失败、队列接近上限、spool 损坏、异常重启和钉钉投递恢复都会进入同一持久队列。SIGHUP 只允许更新主机标识、采样周期、告警策略、日报时间和 checks；`state_dir`、日志级别、命令超时和 delivery 变化会拒绝整次热加载。

## 消息示例

```text
🔴 CRITICAL · 告警

主机：bybit-sg

IP：203.0.113.10

系统主机：ip-10-0-0-12.internal

实例：machine=6a61c41c93b2 boot=ea42b07bd751 pid=1682174 config=8e1f149f138a

检查：bybit-book

状态：SHM 已 180 秒没有推进

异常开始：2026-08-12 14:31:20

对象：/shm_bybit_lin_book_tick_v2

进度：812945

处理：https://runbook.example/shm-stale
```

## 首次上线

先在非关键主机与旧 monitor 并行运行 24 小时，并使用测试钉钉群。确认异常、恢复、日报和网络中断补发后，再逐机切换；不要让两套 monitor 同时向正式群发送同一规则。
