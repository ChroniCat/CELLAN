# 打包说明

发行给同事的 exe 用静态 CRT 构建，单文件、无需 VC++ 运行库：

```bash
RUSTFLAGS="-C target-feature=+crt-static" \
  cargo build --release --offline -p app --no-default-features --features glow
cp target/release/app.exe dist/SheetsNative.exe
```

- release 版通过 `#![cfg_attr(..., windows_subsystem = "windows")]` 使用 GUI 子系统，双击不弹控制台。
- `使用说明.txt`、`THIRD_PARTY_NOTICES.txt` 随 exe 一起分发（字体 OFL、Rust 依赖许可）。
- 打成 zip 时把 `SheetsNative.exe / SheetsNative.ico / 使用说明.txt / THIRD_PARTY_NOTICES.txt` 放一起。
- 目标机器要求：Windows 10/11 x64 + OpenGL 3.3。
