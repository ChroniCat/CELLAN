# AGENTS.md — CELLAN 开发、测试与一键发版规范

本文件供所有在本仓库工作的 AI 编程助手（GPT / Codex、Claude Code、Gemini / Antigravity 等）共同遵守。

---

## 1. 仓库与工具链环境（必读）

- **项目定位**：`CELLAN` 是纯 Rust 编写的 Windows 原生单文件电子表格桌面应用（`x86_64-pc-windows-msvc`），包含两个 crate：
  - `crates/core`（包名 `sheetcore`）：表格核心引擎、公式计算、OOXML/BIFF8 读写、局域网协同协议（`sync.rs`）。
  - `crates/app`（包名 `app`，产物 `CELLAN.exe`）：Windows GPU 原生界面（`wgpu` + `glow`）、局域网实时协作（`net.rs`）、内网签名自更新（`update.rs`）、内置 AI 助手（`ai_agent.rs`）。
- **工具链位置（重要）**：
  - 本仓库使用 **Windows 原生 MSVC Rust 工具链**（位于 Windows 侧 `%USERPROFILE%\.cargo\bin\cargo.exe`，仓库 Windows 路径为 `F:\finance-native-sheets-ready`，WSL 挂载路径为 `/mnt/f/finance-native-sheets-ready`）。
  - **若当前 Agent 运行在 WSL / Linux Bash 环境**：必须通过 `powershell.exe` 调用 Windows 侧的 `cargo` 与脚本，并始终附带 `-NoProfile -NonInteractive -InputFormat None` 及 `< /dev/null` 防止标准输入挂起。
  - 优先追加 `--offline` 使用本地依赖缓存构建与测试。
  - **目录收敛约束**：所有开发、测试与发布 worktree 统一创建在本仓库根目录的 `.worktrees/` 下（已在 `.gitignore` 中忽略），**严禁**在仓库外部（如 `../LANCell-*` 或 `F:\` 等盘符根目录）散落临时目录。常规开发测试如需跨 worktree 复用编译缓存可按需设置 `CARGO_TARGET_DIR`；正式发版由 `scripts/publish.ps1` 自动使用发布 worktree 自身的 `target`。

### 每个任务使用独立 worktree

- 开始编辑前，先用 `git worktree list` 查看已有工作目录，用 `git status --short --branch` 检查当前目录。每个 AI 任务使用专属 worktree 和分支；续做同一任务时复用自己的 worktree。
- 新任务默认从最新的远端 `main` 开始。在主仓库目录运行 `git fetch origin main`，再运行 `git worktree add --no-track -b codex/<任务名> .worktrees/<任务名> origin/main`。将占位符替换为实际名称，后续编辑、测试和提交都在 `.worktrees/<任务名>` 中进行，不要在仓库外部新建目录。用户指定基线时遵照指定基线。
- 不在其他 AI 或用户使用的目录切换分支、暂存、提交、覆盖、还原或清理文件。不要对共享工作目录执行 `git stash`、`git reset --hard` 或 `git clean`，也不要移除其他未完成任务的 worktree。
- 自己的任务分支合入 `main` 并确认不再需要后，先核对工作区干净且无进程占用，再在主仓库执行 `git worktree remove .worktrees/<任务名>` 及时清理，避免 `target/` 和旧目录堆积。
- 开工时说明自己负责的文件或模块。若同一文件已由其他 AI 修改，先协调归属和集成顺序；独立 worktree 只能隔离磁盘写入，合并时仍可能有逻辑冲突。等待协调期间可以继续处理互不重叠的文件。

### 集成到 main

- 在任务分支审阅差异、运行相关测试和构建，仅提交本任务的改动，并推送任务分支。交接时报告分支名、提交号、验证结果，以及是否已合入远端 `main`。
- 由负责集成的 AI 或维护者将通过验证的任务分支合入 `main`。如果其他任务已经更新 `main`，先核对合并结果，再运行受影响的测试。保留其他任务的提交历史，不强制推送覆盖远端。

### 常用测试命令

以下命令均从当前任务的 worktree 根目录运行。

- **在 Windows PowerShell 中**：
  ```powershell
  $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
  cargo test --offline -p sheetcore
  cargo test --offline -p app --no-default-features --features glow --bin app <测试过滤名>
  ```
- **在 WSL / Bash 中**：
  ```bash
  powershell.exe -NoProfile -NonInteractive -InputFormat None -Command '$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"; cargo test --offline -p app --no-default-features --features glow --bin app <测试过滤名>' < /dev/null
  ```
- **测试隔离约束**：客户端测试必须使用 `crates/app/src/test_env.rs` 中的 `TestEnv` / `scratch` 隔离 `TEMP` 与 `LOCALAPPDATA`，严禁污染本机真实的 `%LOCALAPPDATA%\CELLAN`。

---

## 2. 如何发版 / 打包更新（AI Agent 标准流程）

当用户要求 **“发版”、“发布新版本”、“打包更新”、“打个新包”**，或在改完需求/Bug 后要求直接发布给同事时，**必须且只能通过 `scripts/publish.ps1` 完成**。

### 发布前选定 main 提交

1. 先将本次要交付的代码、发版脚本和“关于”显示提交号的实现合入远端 `main`，通过相关测试。构建时从 Git 取得提交号并嵌入 exe；无法取得时停止发布。
2. 在主仓库目录运行 `git fetch origin main` 和 `git rev-parse origin/main`，记录完整提交号。用 `git worktree add --detach .worktrees/release-<短提交号> <完整提交号>` 在项目内部创建专用发布目录（不要建在仓库外的 `F:\` 根目录）。进入该目录，核对 `git rev-parse HEAD` 与记录一致，且 `git status --porcelain` 输出为空。
3. 在 `.worktrees/release-<短提交号>` 根目录调用下文的脚本，使用该目录自己的 `target` 和 `dist`。脚本会在编译前同步版本号及签名公钥；正式发布应先将目标版本、对应公钥和依赖锁文件的必要变更合入 `main`，再显式传入相同版本，确保实际编译的源码与记录的提交一致。私钥始终留在仓库外。

### 严禁手工裸拷 `target/release/app.exe`
**不要**只跑 `cargo build --release` 然后手动复制 `target/release/app.exe` 到 `dist/CELLAN/CELLAN.exe`！原因：
1. 裸编译产物未注入默认内网更新地址（`CELLAN_DEFAULT_UPDATE_URL`）；
2. 未生成 ECDSA P-256 数字签名清单 `manifest.txt`；
3. 未写入 `CELLAN.exe` 尾部的 `CLNUPD01` 签名标记（缺少该标记将无法在局域网实时协作中给其他同事接力供种）；
4. 未同步热更新本机后台静默发版目录 `%LOCALAPPDATA%\CELLAN\publish-server\`。

### 标准一键发版命令

发版前请先确保相关测试通过，并准备好一句**面向普通用户的简明中文更新说明**（同事客户端收到更新弹窗时会直接展示这段文字）。

#### 方式 A：当前 Shell 为 Windows PowerShell / CMD
```powershell
# 自动将当前次版本号 +1（例如 1.70 -> 1.71），并附上中文更新说明（脚本支持的自动升版用法；正式发布使用下方显式版本命令）：
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\publish.ps1 "" "修复透视表刷新问题，优化筛选性能"

# 或显式指定目标版本号（两段式 X.YY）：
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\publish.ps1 1.71 "修复透视表刷新问题，优化筛选性能"
```

#### 方式 B：当前 Shell 为 WSL / Bash（在发布 worktree 根目录运行）
```bash
# 自动将当前次版本号 +1（第一参数传空串 ''）：
powershell.exe -NoProfile -NonInteractive -InputFormat None -ExecutionPolicy Bypass -File "$(wslpath -w "$PWD/scripts/publish.ps1")" '' '修复透视表刷新问题，优化筛选性能' < /dev/null

# 或显式指定目标版本号（例如 1.71）：
powershell.exe -NoProfile -NonInteractive -InputFormat None -ExecutionPolicy Bypass -File "$(wslpath -w "$PWD/scripts/publish.ps1")" '1.71' '修复透视表刷新问题，优化筛选性能' < /dev/null
```
*(若更新说明较长或包含引号等特殊字符，也可先写入 UTF-8 临时文件，再传 `-NotesFile 'F:\...\notes.txt'`。)*

---

## 3. `scripts/publish.ps1` 自动完成的事项与注意事项

运行 `scripts/publish.ps1` 会全自动执行以下步骤，Agent **无需手动重复修改版本号或重启服务**：

1. **自动同步全仓版本号**：
   - `crates/app/src/update.rs` 中的 `pub const VERSION: &str = "X.YY";`（两段式，如 `"1.71"`）。
   - `Cargo.toml`、`crates/core/Cargo.toml`、`crates/app/Cargo.toml` 中的 `version = "X.YY.0"`（三段式，必须以 `"{VERSION}."` 开头，单测 `version_matches_cargo_package_and_migrates_legacy_profile` 会强制校验两者一致）。
   - `packaging/使用说明.txt` 顶部的 `版本：X.YY`。
2. **自动加载/初始化官方签名密钥**：
   - 私钥保存在本机 `%USERPROFILE%\.cellan-release\signing-key.blob`（位于仓库外部，且已在 `.gitignore` 中忽略）。
   - **严禁删除、覆盖、重置或提交 `%USERPROFILE%\.cellan-release\signing-key.blob`！** 一旦更换密钥，已分发给同事的旧客户端将因公钥不匹配而拒绝自动升级。
   - 脚本会自动核对并确保 `crates/app/src/update.rs` 中的 `DEFAULT_PUBKEY_HEX` 与本机私钥对应。
3. **自动探测本机内网 IP 并编译静态单文件版**：
   - 自动获取当前机器默认路由网卡的局域网 IPv4（默认端口 `48768`），通过环境变量 `CELLAN_DEFAULT_UPDATE_URL=http://<LAN_IP>:48768` 编译进 `CELLAN.exe`（不把机器私有 IP 写死进源码）。
   - 启用 `+crt-static` 与 `--features wgpu,glow` 编译免 VC++ 运行库的单文件 `CELLAN.exe`。
4. **数字签名与自包含尾部封装（`CLNUPD01`）**：
   - 计算原始二进制的字节大小与 SHA-256，使用本机 ECDSA P-256 私钥签名生成 `manifest.txt`，并将 `manifest + 长度 + CLNUPD01` 追加到 `dist/CELLAN/CELLAN.exe` 末尾。
5. **热发布至本机后台静默 HTTP 服务**：
   - 原子替换 `%LOCALAPPDATA%\CELLAN\publish-server\CELLAN.exe` 与 `manifest.txt`（后台发版进程每次请求按需读盘，热替换即刻生效）。
   - 若后台静默服务 `cellan-update-server.exe` 未运行，脚本会自动无窗口拉起、注册 `HKCU\...\Run\CELLANUpdateServer` 开机自启，并在管理员权限下放行内网 TCP `48768` 防火墙规则。

### 发版成功的判定标准

脚本退出码为 `0`，且终端末尾输出如下两行即代表发版与内网热部署全部就绪：
- `[serve] Live at http://<内网IP>:48768/manifest.txt (silent background daemon active)`
- `Published CELLAN v<版本号> successfully!`

内网服务就绪后，还须验收实际交付的文件：

1. 从发布 worktree 的 `.worktrees/release-<短提交号>/dist/CELLAN/CELLAN.exe` 启动已签名的程序，打开“关于”。产品版本须与脚本的目标版本一致，Git 短提交号须至少 7 位，并与第 2 节记录的完整提交号前缀一致。任一项缺失或不匹配时，停止交付并报告差异。验收完成后关闭该测试进程。
2. 将验收通过的签名发布包（`dist/CELLAN/` 下完整文件，含 `CELLAN.exe`、`manifest.txt`、`版本.txt` 等）归档至**主仓库**的 `dist/<短提交号>/`，并同步更新主仓库的 `dist/CELLAN/`（同时保留内网服务路径 `%LOCALAPPDATA%\CELLAN\publish-server\`）。
3. 确认归档完成且无进程占用后，在主仓库执行 `git worktree remove --force .worktrees/release-<短提交号>` 清理临时发布目录（仅保留主仓库 `dist/` 中的归档产物）。
4. 交接时报告产品版本、完整 Git 提交号、主仓库归档产物路径和验证结果。
