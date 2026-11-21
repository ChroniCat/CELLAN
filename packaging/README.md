# 打包与一键发版说明

## 一键签名发版（推荐）

在 Windows PowerShell 中，先用准备模式同步目标版本和签名公钥。将准备好的变更合入主干后，从该提交的干净发布目录运行打包命令。程序的“关于”窗口显示版本和 Git 提交号。

```powershell
.\scripts\publish.ps1 1.71 -PrepareOnly
```

只制作签名 EXE 时使用 `-NoServe`。此模式使用本机 Cargo 缓存，在 `dist/CELLAN/` 生成 EXE 和清单。若要在程序中预置更新地址，可同时传入 `-UpdateUrl`。地址只写入产物；打包过程不会探测网络、修改后台服务目录、注册开机项或更改防火墙。

```powershell
.\scripts\publish.ps1 1.71 "修复透视表刷新问题，优化大数据筛选" -NoServe
```

需要在局域网正式发布时运行：

```powershell
.\scripts\publish.ps1 1.71 "修复透视表刷新问题，优化大数据筛选"
```

1. **签名密钥**：仅在首次准备且仓库尚未固定公钥时创建 `%USERPROFILE%\.cellan-release\signing-key.blob`。私钥保存在仓库外，请妥善备份。发版时会校验其公钥与源码中的记录。
2. **自动读取当前内网 IP**：自动识别本机主网卡局域网 IPv4（如 `http://192.168.x.x:48768`）并编译进 `CELLAN.exe`。
3. **自包含签名包（`CLNUPD01`）**：将签名清单嵌入 `dist/CELLAN/CELLAN.exe` 尾部，同时输出 `dist/CELLAN/manifest.txt`。
4. **后台静默供种服务**：将新版同步至 `%LOCALAPPDATA%\CELLAN\publish-server`，并在后台静默拉起无窗口的内置静态发版服务（开机自启，即使关闭表格主窗口也能随时响应同事更新）。
5. **双通道无感升级**：
   - **主通道（直连发版机）**：同事打开 `CELLAN` 时后台异步检查内置发版地址，发现新版弹窗一键下载、公钥验签、原地替换正在运行的 `CELLAN.exe` 并自动重启。
   - **接力通道（协作顺风车）**：若发版机临时关机，只要任一同事已升级至签名新版，其他同事在与他进行「局域网实时协作」时也会自动收到经公钥校验的更新提示并可一键接力升级（复用协作连接，不新增监听端口、不额外触发防火墙弹窗）。

---

## 手动构建单文件版（仅本地调试打包）

分发的单文件版 exe 采用静态 CRT + `wgpu/glow` 双渲染后端构建，无需安装 VC++ 运行库：

```powershell
$env:RUSTFLAGS = "-C target-feature=+crt-static"
cargo build --release --offline -p app --features wgpu,glow
```

- release 版通过 `#![cfg_attr(..., windows_subsystem = "windows")]` 使用 GUI 子系统，双击不弹控制台；若图形初始化或主线程发生异常，会弹出中文提示并记录到 `%LOCALAPPDATA%\CELLAN\crash.log`。
- `assets/app.ico` 已通过 `build.rs` 内嵌进 `CELLAN.exe` 资源表，分发包中不再附带单独的 `.ico` 文件以免非技术用户误点。
- `使用说明.txt`、`THIRD_PARTY_NOTICES.txt` 随 exe 一起分发（字体 OFL、Rust 依赖许可）。
- 目标机器要求：Windows 10/11 x64（优先使用 OpenGL 3.3 `glow` 硬件加速，不支持时自动降级到 `wgpu` DirectX 12 / WARP 软件渲染）。
