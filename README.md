# Sheets Native

面向 Windows 的 Rust 原生电子表格桌面应用，包含桌面界面（`crates/app`）和表格核心（`crates/core`）。

## 构建

需要 Rust 工具链及 Windows C++ 构建工具。在仓库根目录执行：

```powershell
cargo build -p app --no-default-features --features glow
```

发行版、静态运行库和目标机器要求见 [打包说明](packaging/README.md)。首次构建需要获取 Cargo 依赖；已有完整依赖缓存时可增加 `--offline`。

## 测试

```powershell
cargo test -p sheetcore
cargo test -p app --no-default-features --features glow
```

部分集成测试依赖未随仓库提供的工作簿，默认标记为 ignored；准备相应样本后需显式运行。测试使用独立的空 TEMP/TMP 目录；GUI 与协作测试可能启动程序或监听本地网络。具体步骤见 [测试说明](crates/core/tests/README.md)。开发记录见 [原生重写记录](docs/native-rewrite.md) 与 [界面差异清单](docs/ui-parity-audit.md)。

## 第三方资源

字体和依赖许可见 [第三方声明](packaging/THIRD_PARTY_NOTICES.txt)。部分界面图标来自 Office `GetImageMso`，分发前应核实适用授权。
