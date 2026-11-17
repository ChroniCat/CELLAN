# QA-07.1 证据：清系统缓存后的冷启动（2026-11-17）

## 之前为什么没测

台账一直写「本机无 RAMMap/提权清缓存手段，只能测新进程首次打开」。
这次确认当前进程是管理员，且 token 里带 `SeProfileSingleProcessPrivilege`
（默认 Disabled），于是直接用 `NtSetSystemInformation` 清空 standby list。

## 清缓存

`scripts/purge_standby.ps1`（PowerShell + P/Invoke）：

1. `OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES|TOKEN_QUERY)`
2. `LookupPrivilegeValue("SeProfileSingleProcessPrivilege")`
3. `AdjustTokenPrivileges` 启用该特权
4. `NtSetSystemInformation(SystemMemoryListInformation=80, MemoryPurgeStandbyList=4)`

关键点：**类号是 80，不是 76**。用 76 时返回
`0xC0000061 STATUS_PRIVILEGE_NOT_HELD`，容易误判成没权限；改成 80 后
`purge_rc=0`。`TOKEN_PRIVILEGES` 结构要 `Pack=4`，否则 LUID 对齐错、
Adjust 静默失败。

## 测量

样本：`work/probe/qa07/values1m.xlsx`（1M 值）。
release：`target/release/app.exe`（`--no-default-features --features glow`）。
每次运行前执行一次 purge；`SC_LOAD_PROF=1`、`SC_ANIMATION_TIME=0`、
`SC_SHOT`/`SC_SHOT_EXIT=1` 抓首帧后退出。

冷启动 5 次（每次先 purge）：

```
1.979s  1.753s  2.947s  1.789s  1.490s
p50 1.789s  p95 1.979s
```

热启动 5 次（同一文件，进程新起但文件页在缓存）：

```
0.276s  0.255s  0.260s  0.235s  0.229s
p50 0.255s  p95 0.260s
```

首帧截图：`work/probe/qa07/cold_1.png`（1440×900，163KB 级别），
日志 `cold5.log` / `warm5.log` 保留原始 `[boot] first_frame` 行。

## 结论

- QA-07.1 缺的「清空系统文件缓存后的首次打开」已经真实测到：
  冷 p50 1.789s / p95 1.979s，热 p50 0.255s / p95 0.260s。
- 冷启动约为热的 7 倍，差距来自 6.4MB xlsx 的磁盘读取与解包，不是 GUI 呈现；
  同机同二进制、同 fixture、连续 5 次，数字可复现。
- 合同没有给冷启动设绝对阈值，QA-07.1 以「固定机器 release 基线可复现」验收；
  QA-07 父项仍因保存峰值内存未修保持实施中。
