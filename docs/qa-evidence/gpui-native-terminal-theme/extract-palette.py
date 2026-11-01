"""Read center pixels from capture-palette.mts output. Run from the checkout root."""
import hashlib
import json
import sys
from pathlib import Path
from PIL import Image

output = Path(sys.argv[1])
root = Path.cwd()
metadata = json.loads((output / "style-palette-pages.json").read_text())
width = metadata["dimensions"]["css"]["cell"]["width"]
height = metadata["dimensions"]["css"]["cell"]["height"]
cases = []
for page in metadata["pages"]:
    image = Image.open(page[0]["image"]).convert("RGB")
    for case in page:
        def color(x):
            r, g, b = image.getpixel((int(x), int(case["row"] * height + height / 2)))
            return r << 16 | g << 8 | b
        cases.append({"name": case["name"], "data": case["data"],
                      "foreground": color(width / 2), "background": color(width * 1.5)})
sources = [
    "packages/app/src/styles/theme.ts",
    "packages/app/src/terminal/runtime/terminal-font.ts",
    "packages/app/src/terminal/runtime/terminal-emulator-runtime.ts",
    "packages/app/src/hooks/use-settings/storage.ts",
    "packages/app/node_modules/@xterm/addon-webgl/src/TextureAtlas.ts",
    "packages/app/node_modules/@xterm/addon-webgl/src/CellColorResolver.ts",
    "packages/app/node_modules/@xterm/xterm/src/browser/services/CharSizeService.ts",
    "packages/app/node_modules/@xterm/xterm/src/browser/services/MouseCoordsService.ts",
    "packages/app/node_modules/@xterm/xterm/src/common/services/OptionsService.ts",
    "packages/app/node_modules/@xterm/addon-webgl/src/WebglRenderer.ts",
]
result = {
    "source": "Real Electron + original TerminalEmulatorRuntime WebGL, center pixels of full block plus adjacent blank cell",
    "theme": metadata["theme"], "cases": cases,
    "sources": {name: hashlib.sha256((root / name).read_bytes()).hexdigest() for name in sources},
}
(output / "terminal-palette.json").write_text(json.dumps(result, indent=2) + "\n")
print(f"Extracted {len(cases)} real-renderer color pairs")
