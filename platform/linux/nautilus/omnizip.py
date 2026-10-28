# OmniZip context menu for Nautilus (GNOME Files), through nautilus-python
# (docs/07-shell-integration.md §5.2).
#
# The menu itself (which items a selection gets, their titles in the user's language, a submenu
# or a flat list, the items turned off in the settings) comes from `omnizip shell-menu`, by the
# same rules as the Windows menu. Choosing an item starts `omnizip-desktop --shell <verb>` with
# the selection in a list file, as the Windows extension does. OmniZip installs this file
# (settings, context menu) with the paths of its programs filled in below.

import json
import os
import subprocess
import tempfile

from gi.repository import GObject, Nautilus

CLI = "@CLI@"
APP = "@APP@"


def _menu(paths):
    """The items for a selection, or None when OmniZip cannot tell."""
    try:
        out = subprocess.run(
            [CLI, "shell-menu"],
            input="\n".join(paths),
            capture_output=True,
            text=True,
            timeout=3,
        )
        return json.loads(out.stdout)
    except (OSError, ValueError, subprocess.SubprocessError):
        return None


def _run(verb, paths):
    fd, listing = tempfile.mkstemp(prefix="omnizip-shell-", suffix=".txt")
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        f.write("\n".join(paths) + "\n")
    subprocess.Popen(
        [APP, "--shell", verb, "--cwd", os.path.dirname(paths[0]), "--files-from", listing],
        start_new_session=True,
    )


class OmniZipMenu(GObject.GObject, Nautilus.MenuProvider):
    def get_file_items(self, *args):
        # Nautilus 43 and later pass the files only; earlier versions the window first.
        files = args[-1]
        paths = [f.get_location().get_path() for f in files]
        # Files that are not on a local disk (network places, trash) get no menu.
        if not paths or None in paths:
            return []
        menu = _menu(paths)
        if not menu or not menu.get("items"):
            return []
        items = []
        for entry in menu["items"]:
            verb = entry["verb"]
            item = Nautilus.MenuItem(name=f"OmniZip::{verb}", label=entry["title"])
            item.connect("activate", lambda _item, verb=verb: _run(verb, paths))
            items.append(item)
        if menu.get("flat"):
            return items
        root = Nautilus.MenuItem(name="OmniZip::menu", label=menu.get("root") or "OmniZip")
        submenu = Nautilus.Menu()
        for item in items:
            submenu.append_item(item)
        root.set_submenu(submenu)
        return [root]
