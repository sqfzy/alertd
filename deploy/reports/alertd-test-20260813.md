# 部署简报：ip-172-31-6-149.ap-southeast-1.compute.internal · 成功

- **状态：** 成功
- **模式：** `interactive`
- **目标：** `root@52.221.32.231:22`
- **主机：** `ip-172-31-6-149.ap-southeast-1.compute.internal`
- **健康观察：** 通过
- **生成时间：** 2026-08-13T11:27:14.227344+00:00

## 机器基础信息

| 字段 | 值 |
| --- | --- |
| 主机名 | ip-172-31-6-149.ap-southeast-1.compute.internal |
| 操作系统 | Amazon Linux 2023.12.20260611 |
| 内核 | Linux 6.1.174-217.345.amzn2023.aarch64 #1 SMP Mon Jun  1 16:38:41 UTC 2026 GNU/Linux |
| 架构 | aarch64 |
| CPU 型号 | AWS Graviton3 |
| 逻辑 CPU | 4 |
| AWS 区域 | ap-southeast-1 |
| 实例 ID | i-0407e0f09835d2a52 |
| 实例类型 | c7g.xlarge |

### IP、网卡与路由

| 网卡 | 状态 | MAC | 地址 | MTU |
| --- | --- | --- | --- | --- |
| lo | UNKNOWN | 00:00:00:00:00:00 | 127.0.0.1/8, ::1/128 | 65536 |
| ens5 | UP | 0a:59:8f:5a:7d:e5 | 172.31.6.149/20, fe80::859:8fff:fe5a:7de5/64 | 9001 |
| docker0 | DOWN | 02:42:c3:00:3f:ce | 172.17.0.1/16, fe80::42:c3ff:fe00:3fce/64 | 1500 |
| ens7 | UP | 0a:10:ac:ad:c1:af | 172.31.4.190/20, fe80::810:acff:fead:c1af/64 | 9001 |
| tun0 | UNKNOWN |  | 192.168.253.42/32, fe80::c717:fec9:5ff9:d34b/64 | 1500 |

**公网 IP：** 46.137.244.150, 52.221.32.231, 54.179.254.205

### AWS ENI

| 设备序号 | ENI | MAC | 内核接口 | 私网 IP | 公网 IP | 子网 |
| --- | --- | --- | --- | --- | --- | --- |
| 2 | eni-021e4ef34102dde4f | 0a:10:ac:ad:c1:af | ens7 | 172.31.4.190 | 46.137.244.150 | subnet-0d3f3477ae3c6dd78 |
| 0 | eni-00f586b2493379055 | 0a:59:8f:5a:7d:e5 | ens5 | 172.31.6.149 | 52.221.32.231 | subnet-0d3f3477ae3c6dd78 |
| 1 | eni-06f65d492c2b73533 | 0a:99:7f:d1:3d:cd | 内核不可见/可能已解绑 | 172.31.11.92 | 54.179.254.205 | subnet-0d3f3477ae3c6dd78 |

| 目的 | 网关 | 网卡 | Metric |
| --- | --- | --- | --- |
| default | 172.31.0.1 | ens5 | 512 |
| default | 172.31.0.1 | ens7 | 514 |
| 10.0.0.0/8 | 192.168.253.41 | tun0 | — |
| 172.14.0.0/16 | 192.168.253.41 | tun0 | — |
| 172.15.0.0/16 | 192.168.253.41 | tun0 | — |
| 172.16.0.0/12 | 192.168.253.41 | tun0 | — |
| 172.17.0.0/16 | — | docker0 | — |
| 172.31.0.0/20 | — | ens5 | 512 |
| 172.31.0.0/20 | — | ens7 | 514 |
| 172.31.0.1 | — | ens5 | 512 |
| 172.31.0.1 | — | ens7 | 514 |
| 172.31.0.2 | — | ens5 | 512 |
| 172.31.0.2 | — | ens7 | 514 |
| 192.168.253.0/24 | 192.168.253.41 | tun0 | — |
| 192.168.253.41 | — | tun0 | — |

### 业务服务网卡归属

| 服务 | 网卡 | 证据 | 依据 |
| --- | --- | --- | --- |
| bybit-book-shm-creator.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-bridge.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-dedup-creator.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-depth-shm-creator.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-dns.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-dpdk-host-setup.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-dpdk-primary.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-event-shm-creator.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-ftn-consumer.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-lat-consumer.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-lat-precise.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-market-dump-archive.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-perp-order.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-perp-read.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-perp-sg-deploy.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-probe-shm-creator.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-request-shm-creator.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-rx-tuning-fatal-collector.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-rx-tuning-primary.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-rx-tuning-reader.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-syminfo.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-trade-shm-creator.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| bybit-trig-shm-creator.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| winner-mounts.service | unknown | unknown | 无可验证的内核 socket 或显式绑定 |
| wsping-monitor.service | ens5 | observed | kernel socket and route |

### CPU 拓扑与业务服务

| CPU | Socket | Core | NUMA | 允许/配置服务 | 采样实际服务 |
| --- | --- | --- | --- | --- | --- |
| 0 | 0 | 0 | 0 | bybit-book-shm-creator.service (configured)<br>bybit-dedup-creator.service (configured)<br>bybit-depth-shm-creator.service (configured)<br>bybit-event-shm-creator.service (configured)<br>bybit-probe-shm-creator.service (configured)<br>bybit-syminfo.service (configured)<br>bybit-trade-shm-creator.service (configured)<br>wsping-monitor.service (observed-effective) | wsping-monitor.service |
| 1 | 0 | 1 | 0 | wsping-monitor.service (observed-effective) | — |
| 2 | 0 | 2 | 0 | wsping-monitor.service (observed-effective) | — |
| 3 | 0 | 3 | 0 | wsping-monitor.service (observed-effective) | — |
| 未运行/不适用 | — | — | — | bybit-bridge.service (inactive/dead)<br>bybit-dns.service (inactive/dead)<br>bybit-dpdk-host-setup.service (inactive/dead)<br>bybit-dpdk-primary.service (inactive/dead)<br>bybit-ftn-consumer.service (inactive/dead)<br>bybit-lat-consumer.service (inactive/dead)<br>bybit-lat-precise.service (inactive/dead)<br>bybit-market-dump-archive.service (inactive/dead)<br>bybit-perp-order.service (inactive/dead)<br>bybit-perp-read.service (inactive/dead)<br>bybit-perp-sg-deploy.service (inactive/dead)<br>bybit-request-shm-creator.service (inactive/dead)<br>bybit-rx-tuning-fatal-collector.service (inactive/dead)<br>bybit-rx-tuning-primary.service (inactive/dead)<br>bybit-rx-tuning-reader.service (inactive/dead)<br>bybit-trig-shm-creator.service (inactive/dead)<br>winner-mounts.service (active/exited) | — |

### 内存与磁盘

| 指标 | 部署前 | 部署后 |
| --- | --- | --- |
| 总内存 | 7.59 GiB | 7.59 GiB |
| 可用内存 | 5.27 GiB | 4.23 GiB |

| 挂载点 | 设备 | 类型 | 部署前已用 | 部署后已用 | 部署后使用率 | 部署后可用 |
| --- | --- | --- | --- | --- | --- | --- |
| / | /dev/nvme0n1p1 | xfs | 35.72 GiB | 35.74 GiB | 90% | 4.19 GiB |
| /boot/efi | /dev/nvme0n1p128 | vfat | 1.37 MiB | 1.37 MiB | 14% | 8.60 MiB |
| /dev | devtmpfs | devtmpfs | 0.00 B | 0.00 B | 0% | 4.00 MiB |
| /dev/hugepages | hugetlbfs | hugetlbfs | 0.00 B | 0.00 B | - | 0.00 B |
| /dev/mqueue | mqueue | mqueue | 0.00 B | 0.00 B | - | 0.00 B |
| /dev/pts | devpts | devpts | 0.00 B | 0.00 B | - | 0.00 B |
| /dev/shm | tmpfs | tmpfs | 7.19 MiB | 7.19 MiB | 0% | 3.79 GiB |
| /home/ec2-user/.bybit_keys | /dev/nvme1n1p1[/home/ec2-user/.bybit_keys] | xfs | 82.38 GiB | 83.24 GiB | 83% | 16.69 GiB |
| /home/ec2-user/.local | /dev/nvme1n1p1[/home/ec2-user/.local] | xfs | 82.38 GiB | 83.24 GiB | 83% | 16.69 GiB |
| /home/ec2-user/market_infra-repro | /dev/nvme1n1p1[/home/ec2-user/market_infra-repro] | xfs | 82.38 GiB | 83.24 GiB | 83% | 16.69 GiB |
| /home/ec2-user/vcpkg | /dev/nvme1n1p1[/home/ec2-user/vcpkg] | xfs | 82.38 GiB | 83.24 GiB | 83% | 16.69 GiB |
| /mnt/jt | /dev/nvme1n1p1 | xfs | 82.38 GiB | 83.24 GiB | 83% | 16.69 GiB |
| /mnt/jt/docker/overlay2/042c526e20ba52c5898aaf18e1a3250aadc5f1e7bc231662966b32c0def6bfab/merged | overlay | overlay | 82.38 GiB | 83.24 GiB | 83% | 16.69 GiB |
| /mnt/jt/docker/overlay2/7d6928a67b1e84bc3de85c75a9ac9b25d5c2d8cbfe40db5bd45a2388cbf6b625/merged | overlay | overlay | 82.38 GiB | 83.24 GiB | 83% | 16.69 GiB |
| /mnt/jt/docker/overlay2/9fc35cc4a61bd4ce6a0a9b1b1cd2c5ab05564df15f888193fd2918680ebc65e9/merged | overlay | overlay | 82.38 GiB | 83.24 GiB | 83% | 16.69 GiB |
| /proc | proc | proc | 0.00 B | 0.00 B | - | 0.00 B |
| /proc/sys/fs/binfmt_misc | binfmt_misc | binfmt_misc | 0.00 B | 0.00 B | - | 0.00 B |
| /run | tmpfs | tmpfs | 10.49 MiB | 10.49 MiB | 1% | 1.51 GiB |
| /run/credentials/systemd-sysctl.service | ramfs | ramfs | 0.00 B | 0.00 B | - | 0.00 B |
| /run/credentials/systemd-tmpfiles-setup-dev.service | ramfs | ramfs | 0.00 B | 0.00 B | - | 0.00 B |
| /run/credentials/systemd-tmpfiles-setup.service | ramfs | ramfs | 0.00 B | 0.00 B | - | 0.00 B |
| /run/docker/netns/default | nsfs[net:[4026531840]] | nsfs | 0.00 B | 0.00 B | - | 0.00 B |
| /run/netns | tmpfs[/netns] | tmpfs | 10.49 MiB | 10.49 MiB | 1% | 1.51 GiB |
| /run/user/0 | tmpfs | tmpfs | 0.00 B | 0.00 B | 0% | 776.75 MiB |
| /sys | sysfs | sysfs | 0.00 B | 0.00 B | - | 0.00 B |
| /sys/firmware/efi/efivars | efivarfs | efivarfs | 0.00 B | 0.00 B | - | 0.00 B |
| /sys/fs/bpf | bpf | bpf | 0.00 B | 0.00 B | - | 0.00 B |
| /sys/fs/cgroup | cgroup2 | cgroup2 | 0.00 B | 0.00 B | - | 0.00 B |
| /sys/fs/fuse/connections | fusectl | fusectl | 0.00 B | 0.00 B | - | 0.00 B |
| /sys/fs/pstore | pstore | pstore | 0.00 B | 0.00 B | - | 0.00 B |
| /sys/fs/selinux | selinuxfs | selinuxfs | 0.00 B | 0.00 B | - | 0.00 B |
| /sys/kernel/config | configfs | configfs | 0.00 B | 0.00 B | - | 0.00 B |
| /sys/kernel/debug | debugfs | debugfs | 0.00 B | 0.00 B | - | 0.00 B |
| /sys/kernel/debug/tracing | tracefs | tracefs | 0.00 B | 0.00 B | - | 0.00 B |
| /sys/kernel/security | securityfs | securityfs | 0.00 B | 0.00 B | - | 0.00 B |
| /sys/kernel/tracing | tracefs | tracefs | 0.00 B | 0.00 B | - | 0.00 B |
| /tmp | tmpfs | tmpfs | 48.90 MiB | 1.36 GiB | 36% | 2.43 GiB |
| /var/lib/nfs/rpc_pipefs | sunrpc | rpc_pipefs | 0.00 B | 0.00 B | - | 0.00 B |

## 代码溯源

| 角色 | 仓库 | Target | Commit | 构建镜像 | 制品 SHA-256 |
| --- | --- | --- | --- | --- | --- |
| application | git@172.18.127.104:hft-exso/alertd.git | feature/tickfeat-chain-monitoring | ce57cced2676aa50953d9e38b2dc6e0b83526e5f | docker.io/library/rust@sha256:8eed1d324a486196374b67025fe0a8a724245c42d0c3687fa8d6fc953228b9e3 | a76bace980756ef2dffde7b4466060249b92ec5322b4068655e1f66d6c0b7d03 |

## 关键配置

| 配置 | 来源 | 最终生效值 |
| --- | --- | --- |
| runtime.interval | deploy/alertd-test.sg.toml | 10s |
| runtime.command_timeout | deploy/alertd-test.sg.toml | 3s |
| alarm.recover_for | deploy/alertd-test.sg.toml | 10s |
| delivery.credentials | /etc/alertd/alertd.env | <redacted> |
| system_tuning.baseline_sha256 | src/collectors/system_tuning.rs | 27c6096d9b907b8207a5d440cce9c6c6ffce63d90a27ea37fc53870261377da8 |

## 部署变更与健康验证

**最终状态：成功**

| 动作 | 路径 | 回滚动作 |
| --- | --- | --- |
| create | /opt/alertd-test | stop alertd.service and remove /opt/alertd-test |
| create | /etc/alertd | stop alertd.service and remove /etc/alertd |
| create | /var/lib/alertd | stop alertd.service and remove /var/lib/alertd |
| create | /etc/systemd/system/alertd.service | disable service, remove unit, daemon-reload |

**健康门禁：** 通过；阶段 `postdeploy`；轮询 16 次。

## 部署复现流程

1. `git fetch origin ce57cced2676aa50953d9e38b2dc6e0b83526e5f`

2. `git checkout --detach ce57cced2676aa50953d9e38b2dc6e0b83526e5f`

3. `docker run --rm -v $PWD:/src -w /src docker.io/library/rust@sha256:8eed1d324a486196374b67025fe0a8a724245c42d0c3687fa8d6fc953228b9e3 sh -c 'apk add --no-cache musl-dev; cargo test --locked; rustup component add clippy; cargo clippy --locked --all-targets -- -D warnings; cargo build --release --locked'`

4. `sha256sum target/release/alertd`

## 回滚流程

1. `systemctl disable --now alertd.service`

2. `rm -f /etc/systemd/system/alertd.service`

3. `systemctl daemon-reload`

4. `rm -rf /opt/alertd-test /etc/alertd /var/lib/alertd`

## 证据说明

- `configured`：来自明确的 systemd 或应用配置。
- `observed`：来自进程、CPU 采样、内核 socket 或路由观测。
- `inferred`：根据启动参数或路由推断，已注明依据。
- `unknown`：证据不足；不会把 DPDK/raw socket 等不可观测路径伪装成确定结果。

> 报告不包含密钥、token、密码、私钥或 webhook secret。外部状态和数据库不在应用级回滚保证范围内。
