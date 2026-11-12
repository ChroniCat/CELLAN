#!/usr/bin/env bash
# 为一次审计/测试运行建立隔离环境（遵循 docs/excel-wps-audit-2026-11-12/hermes-deepseek.md §4）。
#
# 用法（在仓库根目录）：
#   source scripts/audit_env.sh [标签]
#   cargo test -p sheetcore --offline -- --test-threads=1
#
# 效果：
#   * TEMP/TMP 指向 work/hermes-<标签>-<时间>-<pid>（本次运行专用，不碰系统临时目录）
#   * LOCALAPPDATA 指向该目录下的 profile/，程序的 recent.txt/ui.ini/views 都落在这里
#   * 清除所有继承的 SC_* 环境变量（旧 SC_B1/B3/B4 含固定开发目录写入，禁止启用）
#   * AUDIT_RUN_DIR 记录本次目录，供证据脚本引用
set -u

_AUDIT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
_AUDIT_TAG="${1:-check}"
export AUDIT_RUN_DIR="$_AUDIT_ROOT/work/hermes-$_AUDIT_TAG-$(date +%Y%m%d-%H%M%S)-$$"
mkdir -p "$AUDIT_RUN_DIR/profile"

# Windows 程序（cargo/rustc/app.exe）只认 Windows 路径，这里用 F:/... 形式
export TEMP="$(cygpath -m "$AUDIT_RUN_DIR")"
export TMP="$TEMP"
export LOCALAPPDATA="$(cygpath -m "$AUDIT_RUN_DIR/profile")"

# 继承的 SC_* 一律清掉；需要探针的测试必须自己显式设置
while IFS= read -r _v; do
    [ -n "$_v" ] && unset "$_v"
done < <(env | sed -n 's/^\(SC_[A-Za-z0-9_]*\)=.*/\1/p')

echo "AUDIT_RUN_DIR=$AUDIT_RUN_DIR"
echo "TEMP=$TEMP"
echo "LOCALAPPDATA=$LOCALAPPDATA"
echo "SC_* 清空：$(env | grep -c '^SC_' || true) 个残留"
