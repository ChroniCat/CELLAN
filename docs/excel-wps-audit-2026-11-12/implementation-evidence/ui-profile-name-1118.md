# 右上角显示名：从“所有人都是 Administrator”改为本机独立名字

## 现象

Windows 用户名相同（或克隆镜像共用 Administrator）的机器，右上角账户区全部显示同一个
`USERNAME`；协作光标、编辑占用、修订记录也共用这个名字，无法区分参与者。

## 根因

1. 标题栏账户区直接读 `std::env::var("USERNAME")`，没有持久化的显示名；
2. 默认值只有用户名，没有计算机名等可区分信息；
3. 账户按钮被标题栏空白处的拖动交互层覆盖，真实点击打不开任何设置（本轮实测发现并修复）。

## 修改

- `UiPrefs` 新增 `user_name`，写入 `%LOCALAPPDATA%\SheetsNative\ui.ini`；旧配置没有该键时保持兼容。
- 默认显示名改为 `Windows用户名@计算机名`（`SC_USER_NAME` 可覆盖，部署时可统一指定）；
  右上角窄区域用中间省略，保留机器后缀，例如 `Admin...计部-AI`。
- 点击右上角账户打开“账户”窗口，可修改本机显示名；保存后立即用于协作光标、占用卡和修订记录，
  空名字不覆盖当前值，Esc/取消/关闭均可退出。
- 在线改名新增协议消息 `Msg::Rename` 与 `NetEvent::PeerRenamed`：客户端发给宿主，宿主更新
  `peer_names` 后转发给其他客户端；宿主只信握手时的 site，不能借 Rename 改别人的名字。
- 标题栏拖动层避开快速访问按钮、搜索框和账户按钮，修复“账户按钮能看见但点不开”的问题。

## 验证

### 真实窗口

在隔离的 `LOCALAPPDATA` 下启动 `target/debug/app.exe work/probe/qa041_doc1.xlsx`，真实鼠标点击
右上角账户按钮后截图 `ui-profile-name-1118.png`（1440×900）：

- 标题栏显示 `Admin...计部-AI`（用户名 + 计算机名后缀，已能区分同名的 Administrator）；
- 账户窗口完整显示 `Administrator@设计部-AI`，字段、说明、保存/取消和“关于 Sheets Native”均可见；
- 窗口不是 UIA 可枚举的 egui 模态，因此以真实 `PrintWindow` 截图为准。

### 自动化

```text
cargo test -p app --offline --no-default-features --features glow -j 2 --bin app -- --test-threads=1
  → 401 passed; 0 failed; 1 ignored

cargo test -p app --offline --no-default-features --features glow -j 2 --test ui_regression -- --test-threads=2
  → 20 passed; 0 failed

cargo test -p sheetcore --lib --offline -j 2 -- --test-threads=2
  → 320 passed; 0 failed
```

新增覆盖：

- `display_name_is_cleaned_and_elided_without_byte_panics`：按字符清洗/中间省略，中文不按字节 panic，
  默认名保留机器后缀；
- `display_name_is_persisted_and_used_by_a_fresh_app`：保存后新开 `SheetApp` 仍读回同一名字；
- `sc_user_name_overrides_saved_profile_for_deployment`：部署环境变量优先；
- `rename_is_relayed_to_host_and_other_clients`：真实回环宿主 + 两个客户端，确认改名会到宿主并转发给其他客户端。

## 未验证 / 边界

- 本轮只在本机单机验证；没有两台真实物理机/两个真实 Windows 账号的多人现场。
- 默认唯一性依赖 `COMPUTERNAME`；克隆机如果连计算机名也完全相同，仍需在账户窗口手动改，或部署时用
  `SC_USER_NAME` 指定。
- 旧版本客户端收到 `Rename` 会因未知枚举值跳过该消息（不会断线），但不会更新名字；需要新版客户端才能在线改名。
