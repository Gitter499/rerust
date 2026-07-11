# pixel-mcp setup for ReRust

[pixel-mcp](https://github.com/willibrandon/pixel-mcp) exposes Aseprite's
pixel art tools to Cursor via MCP. It is **not** an npm server — it is a
Go binary that shells out to Aseprite.

## Installed

| Item | Location |
|------|----------|
| Source + binary | `~/tools/pixel-mcp/bin/pixel-mcp` |
| Config | `~/.config/pixel-mcp/config.json` |
| Cursor MCP (user) | `~/.cursor/mcp.json` |
| Cursor MCP (project) | `.cursor/mcp.json` |

## Requirements still needed

1. **Aseprite 1.3+** — not installed on this machine. Install from
   [aseprite.org](https://www.aseprite.org/) or Steam, then update
   `aseprite_path` in `~/.config/pixel-mcp/config.json`:

   ```json
   {
     "aseprite_path": "/Applications/Aseprite.app/Contents/MacOS/aseprite"
   }
   ```

2. **Restart Cursor** after installing Aseprite so the MCP server loads.

## Verify

```bash
~/tools/pixel-mcp/bin/pixel-mcp --health
```

Should report healthy once Aseprite path is valid.

## Rebuild pixel-mcp

```bash
cd ~/tools/pixel-mcp && make build
```

## MCP tools (once Aseprite is available)

Key tools: `create_canvas`, `draw_pixels`, `draw_rectangle`, `draw_circle`,
`set_palette`, `export_sprite`, `export_spritesheet`, `apply_shading`,
`draw_with_dither`, and more. See the
[pixel-mcp README](https://github.com/willibrandon/pixel-mcp#available-tools).

## Sprites without pixel-mcp

Initial ReRust sprites were generated programmatically:

```bash
python3 scripts/generate_sprites.py
```

Output: `docs/assets/*.png`. Edit `scripts/generate_sprites.py` ASCII grids
to tweak designs, or import PNGs into Aseprite via pixel-mcp once configured.
