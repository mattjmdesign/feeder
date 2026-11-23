import os.path

app = defines["app"]
format = "ULFO"
filesystem = "HFS+"
files = [app]
symlinks = {"Applications": "/Applications"}
icon = defines["volume_icon"]
background = defines["background"]

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
