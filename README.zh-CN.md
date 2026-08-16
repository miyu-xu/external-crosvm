# crosvm（BSCP 分支）

[English](README.md) | 简体中文

crosvm 是 BSCP 的虚拟机监控器。发布主分支以 Linux/KVM 为参考，同时提供 macOS/HVF 与
Windows/WHPX 主机适配；上层 `virtmgr`/`vm` 负责把 Microdroid 请求转换为显式设备和资源
配置。

主分支不得包含产品专用 UI 或嵌入显示控制。此类集成只允许出现在 `hd-feature`。开发时先
运行目标平台的格式化、单元测试和最小 Microdroid 启动，再启用网络、图形、音频等可选设备。
任何受保护虚拟机请求在主机能力不足时都必须明确失败。
