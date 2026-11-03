#!/usr/bin/env bash
# 把图标和 .desktop 装进用户目录，让 GNOME 的「活动」/dock 能显示出 shotrs 的图标。
# Wayland 下窗口本身没有图标，GNOME 是靠 app_id(shotrs) 去匹配 shotrs.desktop 的 Icon= 的。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
icons="$HOME/.local/share/icons/hicolor"
apps="$HOME/.local/share/applications"

for s in 32 48 64 128 256 512; do
  dir="$icons/${s}x${s}/apps"
  mkdir -p "$dir"
  cp "$here/shotrs-$s.png" "$dir/shotrs.png"
done

# .desktop 里的 Exec 用占位符，装的时候换成这台机器的真实可执行文件路径
bin="${SHOTRS_BIN:-$(cd "$here/.." && pwd)/target/release/shotrs}"
mkdir -p "$apps"
sed "s|@SHOTRS_BIN@|$bin|" "$here/shotrs.desktop" > "$apps/shotrs.desktop"

gtk-update-icon-cache -f -t "$icons" >/dev/null 2>&1 || true
update-desktop-database "$apps" >/dev/null 2>&1 || true
echo "已安装：$icons/*/apps/shotrs.png 与 $apps/shotrs.desktop"
echo "Exec = $bin（换目录后重跑本脚本，或 SHOTRS_BIN=... 指定）"
