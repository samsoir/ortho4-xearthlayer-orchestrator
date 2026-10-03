#!/usr/bin/env python3
"""OXO runner: drives Ortho4XP's build functions with real exit codes.

Reads one JSON object (RunnerInput) from stdin. Writes exactly one result
line to stdout ({"outcome":"ok"} or {"outcome":"failed",...}); everything
Ortho4XP prints goes to stderr. Exit 0 iff ok.
"""
import json
import os
import re
import sys


def parse_tile(s):
    m = re.fullmatch(r"([+-])(\d{2})([+-])(\d{3})", s)
    if not m:
        raise ValueError("bad tile %r, expected +DD+DDD" % s)
    lat = int(m.group(2)) * (-1 if m.group(1) == "-" else 1)
    lon = int(m.group(4)) * (-1 if m.group(3) == "-" else 1)
    if abs(lat) > 90 or abs(lon) > 180:
        raise ValueError("tile out of range %r" % s)
    return lat, lon


def main(out):
    phase = "setup"
    try:
        inp = json.load(sys.stdin)
        lat, lon = parse_tile(inp["tile"])
        task_type = inp["task_type"]
        if task_type not in ("ortho", "overlay"):
            raise ValueError("unknown task_type %r" % task_type)
        config = inp.get("config") or {}

        # Every O4 path resolves against the cwd.
        os.chdir(inp["install_root"])
        sys.path.append(os.path.join(inp["install_root"], "src"))

        phase = "import"
        import O4_File_Names as FNAMES
        sys.path.append(FNAMES.Provider_dir)
        import O4_Imagery_Utils as IMG
        import O4_Vector_Map as VMAP
        import O4_Mesh_Utils as MESH
        import O4_Mask_Utils as MASK
        import O4_Tile_Utils as TILE
        import O4_Overlay_Utils as OVL
        import O4_Config_Utils as CFG  # last: it modifies other modules' variables

        # Both task types read X-Plane's Global Scenery.
        CFG.custom_overlay_src = inp["overlay_src"]

        phase = "initialize_providers"
        IMG.initialize_extents_dict()
        IMG.initialize_color_filters_dict()
        IMG.initialize_providers_dict()
        IMG.initialize_combined_providers_dict()

        if task_type == "ortho":
            phase = "configure"
            tile = CFG.Tile(lat, lon, "")
            tile.default_website = config["provider"]
            tile.default_zl = config["zoom"]
            for k, v in (config.get("raw") or {}).items():
                setattr(tile, k, v)
            for fn in (VMAP.build_poly_file, MESH.build_mesh, MASK.build_masks, TILE.build_tile):
                phase = fn.__name__
                fn(tile)
        else:
            phase = "prepare_overlay_dir"
            os.makedirs(
                os.path.join(FNAMES.Overlay_dir, "Earth nav data", FNAMES.round_latlon(lat, lon)),
                exist_ok=True,
            )
            phase = "build_overlay"
            OVL.build_overlay(lat, lon)
    except BaseException as e:  # SystemExit from O4 included
        reason = "%s: %s" % (type(e).__name__, e)
        out.write(json.dumps({"outcome": "failed", "reason": reason, "phase": phase}) + "\n")
        out.flush()
        return 1
    out.write(json.dumps({"outcome": "ok"}) + "\n")
    out.flush()
    return 0


if __name__ == "__main__":
    # The result line gets its own descriptor; fd 1 (inherited by any
    # subprocess Ortho4XP spawns, e.g. Triangle4XP) is pointed at stderr.
    result_stream = os.fdopen(os.dup(1), "w")
    os.dup2(2, 1)
    sys.stdout = sys.stderr
    code = main(result_stream)
    result_stream.close()
    sys.exit(code)
