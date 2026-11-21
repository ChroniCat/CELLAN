# 打包说明

分发的单文件版 exe 采用静态 CRT + `wgpu/glow` 双渲染后端构建，无需安装 VC++ 运行库：

```bash
RUSTFLAGS="-C target-feature=+crt-static" \
  cargo build --release --offline -p app --features wgpu,glow
mkdir -p dist/CELLAN
cp target/release/app.exe dist/CELLAN/CELLAN.exe
```

- release 版通过 `#![cfg_attr(..., windows_subsystem = "windows")]` 使用 GUI 子系统，双击不弹控制台；若图形初始化或主线程发生异常，会弹出中文提示并记录到 `%LOCALAPPDATA%\CELLAN\crash.log`。
- `assets/app.ico` 已通过 `build.rs` 内嵌进 `CELLAN.exe` 资源表，分发包中不再附带单独的 `.ico` 文件以免非技术用户误点。
- `使用说明.txt`、`THIRD_PARTY_NOTICES.txt` 随 exe 一起分发（字体 OFL、Rust 依赖许可）。
- 打成 zip 时把 `CELLAN.exe / 使用说明.txt / THIRD_PARTY_NOTICES.txt / 版本.txt` 放一起。
- 目标机器要求：Windows 10/11 x64（优先使用 OpenGL 3.3 `glow` 硬件加速，不支持时自动降级到 `wgpu` DirectX 12 / WARP 软件渲染）。
