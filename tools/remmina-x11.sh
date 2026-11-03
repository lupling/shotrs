#!/bin/sh
# 使用独立应用 ID 启动新进程，不向当前 Wayland 实例转交请求，也不主动断开连接。
set -eu

if [ -z "${DISPLAY:-}" ]; then
    printf '%s\n' '没有 X11 / XWayland 显示服务。' >&2
    exit 1
fi
export GDK_BACKEND=x11
export G_MESSAGES_DEBUG=remmina
export WLOG_FILTER="${WLOG_FILTER:-com.freerdp.channels.cliprdr.*:DEBUG,com.freerdp.client.cliprdr.*:DEBUG}"
exec remmina --gapplication-app-id=org.remmina.Remmina.ShotrsX11 "$@"
