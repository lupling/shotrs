"""持有 X11 图像剪贴板，同时由 GTK 提供 PNG、BMP 等可转换格式。

PNG 从 stdin 读取；成功取得所有权后向 stdout 写 READY。替换剪贴板后退出，
进程独立于截图窗口存活。通过 XWayland 也可供 Wayland 应用读取。
"""

import sys


def main():
    import gi

    gi.require_version("Gtk", "3.0")
    gi.require_version("Gdk", "3.0")
    gi.require_version("GdkPixbuf", "2.0")
    from gi.repository import Gdk, GdkPixbuf, GLib, Gtk

    initialized, _ = Gtk.init_check([])
    if not initialized:
        raise RuntimeError("GTK 无法连接 X11 显示服务")

    loader = GdkPixbuf.PixbufLoader.new_with_type("png")
    loader.write(sys.stdin.buffer.read())
    loader.close()
    pixbuf = loader.get_pixbuf()
    if pixbuf is None or pixbuf.get_has_alpha():
        raise RuntimeError("剪贴板输入必须是无 alpha 的 RGB PNG")

    clipboard = Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD)
    clipboard.set_image(pixbuf)
    display = Gdk.Display.get_default()
    display.sync()
    owner = Gdk.selection_owner_get(Gdk.SELECTION_CLIPBOARD)
    if owner is None:
        raise RuntimeError("GTK 未取得剪贴板所有权")

    def changed(_clipboard, event):
        if event.owner != owner:
            Gtk.main_quit()

    def ready():
        if Gdk.selection_owner_get(Gdk.SELECTION_CLIPBOARD) != owner:
            Gtk.main_quit()
            return False
        print("READY", flush=True)
        # 关闭握手管道，避免截图进程退出后仍持有它的输出流。
        sys.stdout.close()
        return False

    clipboard.connect("owner-change", changed)
    GLib.idle_add(ready)
    Gtk.main()


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"ERROR {error}", flush=True)
        sys.exit(1)
