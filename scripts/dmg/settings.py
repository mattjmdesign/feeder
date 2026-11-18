# dmgbuild settings for Feeder's disk image: one window, the app on the left, an arrow, and
# Applications on the right. mac-app.sh passes `app`, `background` and `volume_icon` with -D.
import os.path

app = defines["app"]  # noqa: F821 (dmgbuild provides `defines`)
format = "ULFO"
filesystem = "HFS+"
files = [app]
symlinks = {"Applications": "/Applications"}
icon = defines["volume_icon"]  # noqa: F821
background = defines["background"]  # noqa: F821

window_rect = ((200, 160), (600, 380))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False
icon_size = 112
text_size = 13
icon_locations = {os.path.basename(app): (160, 172), "Applications": (440, 172)}
