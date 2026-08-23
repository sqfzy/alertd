# alertd

`alertd` 是一个轻量、配置驱动的 Linux 告警守护程序。每台机器运行一个实例，监控应用链路和主机运行态；异常、持续、恢复、日志事件和日报统一发送到钉钉。

```text
Collector → Observation → Alarm Engine → Event → Durable Queue → DingTalk
```

一个 Rust 二进制、一份 TOML、一个 systemd unit；不依赖 Prometheus、数据库、中心服务或动态插件。

## 快速开始

```sh
cargo build --release
cp config/alertd.toml.example /etc/alertd/alertd.toml
cp deploy/alertd.env.example /etc/alertd/alertd.env
chmod 0600 /etc/alertd/alertd.env

target/release/alertd --config /etc/alertd/alertd.toml --check-config
target/release/alertd --config /etc/alertd/alertd.toml --dry-run
target/release/alertd --config /etc/alertd/alertd.toml --send-test
```

最小配置：

```toml
[[checks]]
name = "root-disk"
type = "disk"
mount = "/"
warn_used_pct = 80
critical_used_pct = 90
```

完整字段、默认值、范围和各类 check 示例以 [`config/alertd.toml.example`](config/alertd.toml.example) 为准。未知字段、重复 check 名称和非法范围会被拒绝。

## 常用操作

```sh
# 热加载；校验失败时继续使用旧配置
systemctl kill -s HUP alertd

# 设置、查看、取消维护窗口
alertd --config /etc/alertd/alertd.toml maintenance start \
  --until '2026-08-21T16:00:00+08:00' \
  --reason '业务重新部署'
alertd --config /etc/alertd/alertd.toml maintenance status
alertd --config /etc/alertd/alertd.toml maintenance cancel

# 本地日志与服务状态
systemctl status alertd
journalctl -u alertd -n 200 --no-pager
```

进一步阅读：

- [架构与一致性](docs/architecture.md)：状态机、journald 事件、cursor、持久队列和维护窗口的不变量。
- [部署与运维](docs/operations.md)：systemd 安装、热加载边界、维护流程、排障、升级和回滚。
- [完整配置示例](config/alertd.toml.example)：严格 schema 的主要事实入口。

## Check 类型

- `process`：匹配进程命令行和最少实例数。
- `shm`：检查 POSIX SHM 存在性或计数器推进。
- `journal`：按 unit 和普通子串匹配事件，可优先过滤已知噪声。
- `systemd`：检查一组 service/timer 的 loaded、active 状态。
- `latest_file`：检查匹配文件的大小和 mtime 新鲜度。
- `metrics_file`：读取原子覆盖的 JSON 数值快照，检查新鲜度和可选上下限，并纳入日报。
- `metrics_shm`：可选校验 ABI 原始字节，定点读取固定类型数值，检查上下限并纳入日报。
- `disk`：检查挂载点容量与 inode 使用率。
- `memory`：按 `MemAvailable/MemTotal` 检查可用内存。
- `cpu`：连续采样并显示每个逻辑 CPU 的使用率。
- `time_sync`：通过 `chronyc -c tracking` 检查同步和时钟偏差。
- `network`：检查接口链路及 error/drop 每秒速率。
- `system_tuning`：只读检查当前低延迟运行态，不修改主机。

所有 collector 只采集事实。统一告警状态机负责等待、升级、重复和恢复防抖；collector 连续失败会产生独立的采集盲区告警。

`metrics_file` 的生产者负责聚合业务数据，并以“同目录临时文件 + 原子 rename”更新不超过 64 KiB 的顶层 JSON 对象。alertd 只读取配置选中的有限数值，不保存历史或计算 average/max/p99；统计窗口和单位应体现在稳定的 key 名中。

`metrics_shm` 按 `runtime.interval` 打开 POSIX SHM 一次，通过同一文件描述符读取可选 ABI hash 和配置字段；支持大小端整数与浮点数。生产者必须以自然对齐的原子写更新单个字段。alertd 提供单值最佳努力采样，不保证多个字段属于同一事务；需要跨字段一致性时使用原子 JSON 快照，或另行设计 seqlock。

两类数值检查都可独立配置 `critical_below`、`warn_below`、`warn_above` 和 `critical_above`。下限使用 `<=`、上限使用 `>=`，达到边界即越线；四项全空时只进入日报。alertd 不支持表达式、多段区间、跨指标计算或单位换算。

## 文件组织

```text
alertd/
├── src/
│   ├── main.rs             CLI 与进程入口
│   ├── config.rs           严格 TOML 数据模型与校验
│   ├── runtime.rs          采集、热加载、日报与自监控编排
│   ├── maintenance.rs      持久化维护窗口控制
│   ├── alarm.rs            告警和日志事件状态机
│   ├── model.rs            Observation、事件与状态 POD
│   ├── report.rs           告警、内部事件和日报排版
│   ├── collectors/         各 check 的只读事实采集器
│   └── delivery/           持久队列与钉钉投递
├── docs/
│   ├── architecture.md     数据流与一致性不变量
│   └── operations.md       部署、维护、排障与回滚
├── config/                 完整配置示例与部署角色样例
├── deploy/                 systemd unit 与环境文件示例
└── tests/                  配置、collector 与报告集成测试
```

## 本地验收

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```
