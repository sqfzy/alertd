//! alertd 的库入口：严格配置、只读采集、统一告警状态和持久投递。

pub mod alarm;
pub mod collectors;
pub mod config;
pub mod delivery;
pub mod identity;
pub mod maintenance;
pub mod model;
pub mod report;
pub mod runtime;
pub mod state;
pub mod systemd_notify;
