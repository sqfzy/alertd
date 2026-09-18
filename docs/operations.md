# 部署与运维

本文面向 Linux systemd 主机。配置字段与合法范围以 [`../config/alertd.toml.example`](../config/alertd.toml.example) 为准；内部一致性设计见 [`architecture.md`](architecture.md)。

## 安装与启动

目标机需要 systemd、journald 和 glibc；使用 `time_sync` check 时还需要可连接本机 chronyd 的 `chronyc`。

```sh
install -o root -g root -m 0755 target/release/alertd /usr/local/bin/alertd
install -d -o root -g root -m 0700 /etc/alertd
install -o root -g root -m 0600 config/alertd.toml.example /etc/alertd/alertd.toml
install -o root -g root -m 0600 deploy/alertd.env.example /etc/alertd/alertd.env
install -o root -g root -m 0644 deploy/alertd.service /etc/systemd/system/alertd.service
```

编辑环境文件，只保存配置所引用的凭据，不把 token 或 secret 写入 TOML。token 始终必填；加签机器人还需提供 secret，IP 白名单机器人应省略 secret：

```sh
ALERTD_DINGTALK_TOKEN=replace-me
# ALERTD_DINGTALK_SECRET=replace-me
```

`delivery.secret_env` 继续指定可选 secret 的环境变量名：该变量存在且非空时请求携带 `timestamp` 和 `sign`；缺失或为空时只携带 access token。启动 INFO 日志中的 `signing_enabled` 可用于确认实际模式。

启动前执行：

```sh
alertd --config /etc/alertd/alertd.toml --check-config
alertd --config /etc/alertd/alertd.toml --dry-run
alertd --config /etc/alertd/alertd.toml --send-test
systemctl daemon-reload
systemctl enable --now alertd
systemctl status alertd
```

`--check-config` 只验证 TOML，不读取钉钉密钥和 Linux 身份文件。正常运行与 `--dry-run` 仍需要 `/etc/machine-id` 和 `/proc/sys/kernel/random/boot_id`。

## 消息身份

`runtime.host` 是稳定角色名；可选的 `runtime.ip` 是值班人员用于识别或连接实例的地址。IP 不自动探测，避免在多网卡、NAT 或隧道环境选错接口。

外发消息还包含系统 hostname 与 `machine`、`boot`、`pid`、`config` 指纹。前三类哈希分别来自去除结尾空白的 `/etc/machine-id`、`/proc/sys/kernel/random/boot_id` 和原始 TOML 字节；消息显示 SHA-256 前 12 位，启动日志记录完整配置哈希。身份在消息入队时冻结，重启后补发不会改写成新进程身份。

```sh
printf %s "$(cat /etc/machine-id)" | sha256sum
printf %s "$(cat /proc/sys/kernel/random/boot_id)" | sha256sum
sha256sum /etc/alertd/alertd.toml
```

## 热加载

修改 TOML 后：

```sh
alertd --config /etc/alertd/alertd.toml --check-config
systemctl reload alertd
journalctl -u alertd -n 50 --no-pager
```

允许热更新：

- `runtime.enabled`、`runtime.host`、`runtime.ip`、`runtime.interval`
- 全部 alarm policy 与日报时间
- checks 的增加、删除和修改

拒绝整次热加载：

- `runtime.state_dir`
- `runtime.log_level`
- `runtime.command_timeout`
- 全部 `delivery` 字段

禁止项涉及启动期资源或 worker 生命周期，需要通过安全重启生效。任何解析、校验或边界检查失败都会保留旧配置并记录 ERROR，同时尝试发送内部 WARN。

## 全局监控开关

业务维护前先关闭监控并确认配置已经生效：

```sh
# 在 [runtime] 中设置 enabled = false
alertd --config /etc/alertd/alertd.toml --check-config
systemctl reload alertd
systemctl status alertd
journalctl -u alertd -n 50 --no-pager
```

`systemctl status` 显示 `Monitoring disabled`，同时钉钉收到内部 WARN 后，再停止或更新业务。关闭后不运行 collector、告警判断和日报；daemon、watchdog、自监控和关闭前已入队消息的投递继续运行。

维护完成后将 `enabled` 改回 `true`，再次校验并执行 `systemctl reload alertd`。alertd 会清空旧告警和采样基线，从当前时间开始读取 journald，并发送一条内部 OK。关闭期间日志不会补报，关闭前异常也不会产生恢复消息。

该开关没有结束时间或自动恢复能力。操作完成后必须人工开启；`--check-config` 只证明磁盘上的 TOML 合法，最终状态以 `systemctl status`、alertd INFO 日志和钉钉开关通知为准。

## 状态与排障入口

默认 `runtime.state_dir=/var/lib/alertd`：

| 路径 | 用途 | 排障原则 |
|---|---|---|
| `state.json` | check 状态、journal cursor、日报、开关通知与生命周期标记 | 不在线编辑；先停止服务并备份再处理 |
| `spool/*.json` | 待投递消息 | 网络恢复后自动重试；不要批量删除 |
| `spool/quarantine/` | 无法解析的队列文件 | 保留现场并结合 alertd ERROR 分析 |

旧版本遗留的 `maintenance.json` 已不再读取，也不会影响 `runtime.enabled`；确认不再回滚到旧版本后可人工归档或删除。

本地日志统一进入 journald：

```sh
journalctl -u alertd --since '30 min ago' --no-pager
journalctl -u alertd -p warning --no-pager
systemctl show alertd -p ActiveState -p SubState -p NRestarts -p WatchdogTimestamp
```

常见检查顺序：

1. 运行 `--check-config`，确认严格 schema 和范围校验通过。
2. 查看 alertd unit 的 ERROR/WARN，确认不是 collector 命令超时或权限问题。
3. 查看 `runtime.enabled`、`systemctl status` 和最近一次热加载日志，确认监控没有被关闭。
4. 统计 `spool/*.json`，确认是否为钉钉超时、429/5xx 或队列已满。
5. 检查 `spool/quarantine/`；隔离文件不会自动重新投递。
6. 对 journal check，用源服务的 `journalctl -u <unit>` 核对原文、过滤子串和 cursor 行为。
7. 对主机 check，直接检查对应 `/proc`、`/sys`、挂载点、`systemctl show` 或 `chronyc -c tracking`。
8. 对 `metrics_file`，检查文件大小、mtime、JSON 顶层对象和配置 key；生产者应以原子 rename 更新。
9. 对 `metrics_shm`，先核对 `/dev/shm/<name>` 的权限与大小，再按配置 offset、类型、字节序和可选 ABI 原始字节检查生产者布局。ABI 不匹配是对象异常；短读、越界和非法浮点会进入采集盲区路径。

## 安全停止、升级与回滚

停止会阻止新采集、保存状态，并给投递 worker 有界退出时间；不会等待钉钉网络无限恢复：

```sh
systemctl stop alertd
```

升级前保留当前二进制、配置和 unit，先校验新二进制，再替换应用文件：

```sh
cp -a /usr/local/bin/alertd /usr/local/bin/alertd.rollback
cp -a /etc/alertd/alertd.toml /etc/alertd/alertd.toml.rollback
/path/to/new-alertd --config /etc/alertd/alertd.toml --check-config
install -o root -g root -m 0755 /path/to/new-alertd /usr/local/bin/alertd
install -o root -g root -m 0644 deploy/alertd.service /etc/systemd/system/alertd.service
systemctl daemon-reload
systemctl restart alertd
systemctl status alertd
journalctl -u alertd -n 100 --no-pager
```

从维护窗口版本首次升级时必须同步更新 unit，旧 unit 没有 `ExecReload`。若 unit 尚未更新，可临时使用 `systemctl kill -s HUP alertd` 触发相同的配置热加载。

若健康检查失败，停止服务，恢复已知正常的二进制和配置，再启动并核对日志。不要删除 `state_dir`：保留它才能继续使用原 journal cursor 和重试尚未发送的队列消息。若新旧版本的状态兼容性未知，应先复制整个 `state_dir`，再按发布说明处理。

首次上线先使用测试钉钉群，并与旧监控并行观察；正式切换时避免两套系统向同一群发送相同规则。
