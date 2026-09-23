"""Render the app-icon SVGs to the PNGs `tauri icon` consumes.

    python design/render-icons.py
    npm run tauri -- icon design/icon-manifest.json

Needs Chrome or Edge (used headless as the SVG rasterizer). Set CHROME to its
path if it isn't found automatically.
"""

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
SIZE = 1024

# Authored scale in the SVGs, and the scale for Android's adaptive-icon layers.
# Android only guarantees a 66dp circle of the 108dp layer canvas (61%) survives
# every launcher mask; at 0.93 the hourglass's end-plate tips reach 78%. 0.72 puts
# them at ~60%. (`android_fg_scale` in the tauri manifest would be the natural
# fix, but the CLI version in use ignores it -- so the padding is baked in here.)
AUTHORED_SCALE = 'transform="scale(0.93)"'
ANDROID_SCALE = 'transform="scale(0.72)"'

OUTPUTS = [
    # (source svg,               output png,                        android padding?)
    ("app-icon.svg",             "app-icon.png",                    False),
    ("app-icon.svg",             "app-icon-android-fg.png",         True),
    ("app-icon-monochrome.svg",  "app-icon-android-monochrome.png", True),
]

CANDIDATES = [
    os.environ.get("CHROME"),
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    shutil.which("google-chrome"),
    shutil.which("chromium"),
    shutil.which("chromium-browser"),
]


def find_browser() -> str:
    for c in CANDIDATES:
        if c and Path(c).exists():
            return c
    sys.exit("No Chrome/Edge found. Set CHROME=/path/to/chrome and re-run.")


def render(browser: str, svg: str, out: Path, work: Path) -> None:
    # Inlined rather than <img src=...>: headless Chrome blocks file:// subresources,
    # and navigating to a bare .svg applies shrink-to-fit instead of rendering 1:1.
    page = work / "page.html"
    page.write_text(
        "<!doctype html><html><head><meta charset='utf-8'><style>"
        f"html,body{{margin:0;padding:0;width:{SIZE}px;height:{SIZE}px;"
        "overflow:hidden;background:transparent}svg{display:block}"
        f"</style></head><body>{svg}</body></html>",
        encoding="utf-8",
    )
    subprocess.run(
        [
            browser, "--headless", "--no-sandbox", "--disable-gpu",
            "--hide-scrollbars", "--force-device-scale-factor=1",
            "--default-background-color=00000000",
            f"--window-size={SIZE},{SIZE}",
            f"--user-data-dir={work / 'profile'}",
            f"--screenshot={out}",
            page.as_uri(),
        ],
        check=True,
        capture_output=True,
    )
    if not out.exists():
        sys.exit(f"render failed: {out.name}")


def main() -> None:
    browser = find_browser()
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        for src, dst, android in OUTPUTS:
            svg = (HERE / src).read_text(encoding="utf-8")
            if android:
                if svg.count(AUTHORED_SCALE) != 1:
                    sys.exit(f"{src}: expected exactly one {AUTHORED_SCALE}")
                svg = svg.replace(AUTHORED_SCALE, ANDROID_SCALE)
            render(browser, svg, HERE / dst, work)
            print(f"  {dst}")


if __name__ == "__main__":
    main()
