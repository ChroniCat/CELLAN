<div align="center">
  <img src="assets/logo.svg" width="88" height="88" alt="CELLAN Logo" />
  <h1>CELLAN</h1>
  <p><strong>Concurrent Engine for Local-first, Lock-free Async Networks</strong></p>
  <p>面向内网与共享盘场景的 Rust 原生本地优先协同电子表格桌面软件</p>
</div>

---

## 简介 | Overview

**CELLAN**（*Concurrent Engine for Local-first, Lock-free Async Networks*）是一款纯 Rust 编写的 Windows 原生电子表格桌面应用。它针对企业内网、财务核算、部门共享盘（SMB/NAS）与离线局域网环境设计：**无需部署中心服务器、无需云端账号、不依赖 Office 或运行库**，单个可执行文件即可实现高性能电子表格编辑与局域网实时多端协同。

### 核心特性 | Key Highlights

- **Local-First 零配置内网协同**
  - **共享盘协作**：参与者在协作中心输入同一房间口令，可手动建房或加入。启用“自动协作”后，打开共享目录里的同一份普通工作簿会自动发现会话。带打开密码的工作簿需手动共享，修订记录仅保留在本次会话。
  - **无阻塞异步网络与单元格并发协调**：异步消息广播实时同步多端选区光标、增删改与版本修订记录，单元格级编辑租约防止多人并发覆盖冲突。
- **高兼容度原生表格核心（`sheetcore`）**
  - **双向格式保真**：原生读写 `.xlsx` / `.xlsm` / `.xls` / `.csv`，保留未知 OOXML 扩展部件、VBA 宏工程二进制、加密工作簿（Agile / Standard Encryption）、批注、图片与文本框。
  - **完整计算与分析能力**：依赖图驱动的增量/后台异步公式重算、动态数组溢出、数据透视表、图表与迷你图（Sparklines）、条件格式（色阶/数据条/图标集）、数据验证、结构化表格（Table）、分类汇总、单变量求解与页面打印/PDF 排版。
- **单文件免安装原生桌面体验（`app`）**
  - **零外部依赖**：静态链接 CRT 与内嵌字体子集，单 `CELLAN.exe` 双击即用，不依赖 WebView2、.NET 或 VC++ 运行库。
  - **GPU 硬件加速界面**：自适应 wgpu / OpenGL 3.3 渲染管线，支持百万行级虚拟滚动、多工作簿标签页、命令面板模糊检索、高分屏像素对齐矢量图标及 Windows UI Automation（UIA）无障碍树。

---

## 仓库结构 | Workspace Layout

| 目录 | 说明 |
| --- | --- |
| `crates/core` (`sheetcore`) | 表格核心引擎：数据模型、公式解析与异步求值、OOXML/BIFF8 读写、加解密、局域网协同协议与打印排版 |
| `crates/app` (`app`) | Windows 原生桌面客户端：GPU 界面、功能区与对话框体系、单实例转发、共享盘新版本检查（仅提示，不自动安装） |
| `assets/` | 官方矢量品牌标志（`logo.svg`）、多尺寸图标（`app.ico`）与 SIL OFL 内嵌字体子集 |
| `packaging/` | 静态发行版构建指南、用户使用说明与第三方开源组件许可声明 |
| `docs/` | 架构演进记录（[native-rewrite.md](docs/native-rewrite.md)）与 Excel/WPS 对标实测档案 |

---

## 构建与运行 | Build & Run

需要 Rust 工具链（Edition 2024）及 Windows MSVC C++ 构建工具（含 Windows SDK `rc.exe` 用于嵌入图标与版本资源）。

```powershell
# 开发构建（OpenGL 后端）
cargo build -p app --no-default-features --features glow

# 生产单文件静态构建（免 VC++ 运行库）
$env:RUSTFLAGS = "-C target-feature=+crt-static"
cargo build --release -p app --no-default-features --features glow
```

完整打包流程与分发文件清单见 [packaging/README.md](packaging/README.md)。已有完整本地依赖缓存时可追加 `--offline`。

---

## 测试 | Testing

```powershell
# 核心引擎单元与往返集成测试
cargo test -p sheetcore

# 桌面端单元测试与真实进程 UI 回归套件
cargo test -p app --no-default-features --features glow
```

测试通过独立沙盒环境守卫（`TestEnv`）隔离 `TEMP` 与 `%LOCALAPPDATA%\CELLAN`，不会污染本机用户配置。更多样本矩阵测试说明见 [crates/core/tests/README.md](crates/core/tests/README.md)。

---

## 许可证与第三方声明 | License & Notices

本项目基于 [MIT License](LICENSE) 开源。

内嵌字体子集遵循 SIL Open Font License 1.1；Rust 第三方依赖许可全文见 [packaging/THIRD_PARTY_NOTICES.txt](packaging/THIRD_PARTY_NOTICES.txt)。
