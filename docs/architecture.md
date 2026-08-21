# 架构与一致性

本文说明 alertd 的稳定模块边界和不能轻易破坏的一致性约束。配置字段与示例以 [`../config/alertd.toml.example`](../config/alertd.toml.example) 为准，部署操作见 [`operations.md`](operations.md)。

## 数据流

```text
配置 / 状态
     │
     v
Collector → Observation → Alarm Engine → AlertEvent → Durable Queue → DingTalk
    事实          策略与状态转换             原子落盘          网络重试
```

- Collector 只读取当前事实，不决定等待、重复或恢复策略。
- `Observation` 是一次采样结果；普通 check 表示健康状态，journal check 表示一次或一批日志事件。
- Alarm Engine 读取 POD 状态并产生可选 `AlertEvent`。
- 报告层把事件格式化为易读文本，持久队列接受后才算完成本地交付。
- 钉钉 worker 独立重试，不阻塞采集循环。

主要实现位于 `src/collectors/`、`src/model.rs`、`src/alarm.rs`、`src/report.rs`、`src/delivery/queue.rs` 和 `src/delivery/dingtalk.rs`。

## 状态型告警

普通 check 的状态按以下顺序变化：

```text
healthy ──异常──> pending ──pending_for──> firing
   ^                  │                       │
   │                  └──恢复──> healthy      ├──到期──> repeating
   │                                          │
   └──────── recover_for <── recovering <──健康
```

- `pending` 消除短暂毛刺；严重度升级不等待重复周期。
- `firing` 按 WARN/CRITICAL 各自周期重复通知。
- 已触发异常必须连续健康达到 `recover_for` 才恢复；恢复期间再次异常会取消恢复计时。
- Collector 错误不等同于业务对象异常。连续失败达到阈值后，使用独立的 `<check>/collector` 状态报告采集盲区。

## journald 事件语义

journal check 是事件型，不把“本轮没有新日志”解释为恢复：

- `ignore_contains` 先于告警规则执行，均为区分大小写的普通子串匹配。
- 命中按 check/rule 聚合并受重复周期限制；CRITICAL 升级立即通知。
- 被限频的事件仍计入窗口和日报，但不会在后续伪造恢复消息。
- 只有需要发送的事件成功进入持久队列后，才提交该批次的 journal cursor。
- 纯过滤或已被限频的批次已经被逻辑处理，可以提交 cursor。

最后两条共同保证：持久队列拒绝真实告警时不会越过日志；已经明确抑制的事件也不会在下一轮反复读取。

## 持久队列

队列文件先写临时文件并 `fsync`，再原子 rename 并同步目录。只有钉钉确认成功后才删除消息。

因此交付语义是“允许重复，不静默丢失”：网络模糊失败或进程重启可能重复发送，但一条已被队列接受的消息不会仅因进程退出而消失。损坏文件移入 `spool/quarantine/`，避免阻塞其余消息。

普通 check 消息最多占 `queue_capacity - 1` 个槽。最后一个槽只供内部事件使用，使“队列接近或已满”等自监控在业务消息拥塞时仍有机会持久化。

## 维护窗口

维护窗口只抑制 check 告警，不暂停守护程序：

- Collector 继续执行，以更新 journal cursor 以及 CPU、network、SHM 的采样基线。
- 成功或失败的采样都不修改 `CheckState`，维护期间的短暂异常不会留下触发或恢复事件。
- 已入队消息、内部事件、投递 worker、热加载、状态保存和 watchdog 继续运行。
- 日报不发送且不推进日期，窗口结束后的下一轮按原有到期规则补发。

窗口结束时立即恢复检查，不等待钉钉投递；但只有结束通知成功进入持久队列后，才删除 `maintenance.json`。通知 ID 写入状态，用于重启和模糊失败后的去重。窗口文件损坏时 fail-open，并产生去重的内部 WARN，避免错误状态造成长期静默。

## 热加载与状态

SIGHUP 会完整解析和严格校验新 TOML，然后一次性切换。校验失败或禁止热更新的字段发生变化时，旧配置继续运行并产生内部 WARN。具体允许范围见 [`operations.md`](operations.md#热加载)。

`state.json` 保存 check 状态、journal cursor、日报日期、正常关闭标记和维护通知 ID。保存采用临时文件、`fsync`、原子 rename 和目录 `fsync`。旧状态新增字段使用 serde 默认值兼容；状态保存失败会留下本地 ERROR 并进入自监控路径。

正常退出前写入 `clean_shutdown=true`；启动后立即写回 `false`。下一次启动发现明确的 `false` 才报告异常重启，旧状态缺少该字段时保持未知，不制造首次升级噪声。

## 低延迟基线边界

`system_tuning` 只读检查当前生效状态，不执行调优，也不检查 GRUB、sysctl 持久文件或调优 unit。基线固定对应 `lat_tune.sh` SHA-256 `27c6096d9b907b8207a5d440cce9c6c6ffce63d90a27ea37fc53870261377da8`：CPU0 为 housekeeping、其余 present CPU 隔离，并检查内核启动参数、RT throttle、irqbalance 及数据口 IRQ/XPS/RPS。`mitigations=off` 是强制项，会以安全缓解换取延迟，只适用于受控隔离环境。
