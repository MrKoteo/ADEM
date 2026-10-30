from pathlib import Path
import time

EXCLUDE = {"target", ".git", "items"}

def show_tree(path: Path, prefix: str = "") -> None:
    items = list(path.iterdir())
    items.sort(key=lambda item: (not item.is_dir(), item.name))

    for index, item in enumerate(items):
        last = index == len(items) - 1
        branch = "└──" if last else "├──"
        excluded = item.is_dir() and item.name in EXCLUDE
        name = f"{item.name} ..." if excluded else item.name

        print(f"{prefix}{branch}{name}")

        if item.is_dir() and not excluded:
            next_prefix = prefix + ("   " if last else "│ ")
            show_tree(item, next_prefix)

show_tree(Path.cwd())
time.sleep(60)
