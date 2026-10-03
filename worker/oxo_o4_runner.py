#!/usr/bin/env python3
"""OXO runner: drives Ortho4XP's build functions with real exit codes.

Reads one JSON object (RunnerInput) from stdin. Writes exactly one result
line to stdout ({"outcome":"ok"} or {"outcome":"failed",...}); everything
Ortho4XP prints goes to stderr. Exit 0 iff ok.
"""
import ast
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


class BuildFailed(Exception):
    pass


def convert_raw(cfg_vars, key, value):
    """Convert a wire string the way O4_Config_Utils' config reader does:
    bool and list values are evaluated as Python literals, everything else is
    passed through the variable's type. Unknown keys fail loudly."""
    if key not in cfg_vars:
        raise KeyError("unknown config variable %r" % key)
    typ = cfg_vars[key]["type"]
    if typ in (bool, list):
        return ast.literal_eval(value)
    return typ(value)


# Owning-module aliases, as Ortho4XP.py and O4_Config_Utils.py import them,
# keyed by the "module" binding in O4_Cfg_Vars.cfg_app_vars.
APP_MODULES = {
    "UI": "O4_UI_Utils",
    "OSM": "O4_OSM_Utils",
    "IMG": "O4_Imagery_Utils",
    "TILE": "O4_Tile_Utils",
    "OVL": "O4_Overlay_Utils",
}

# The pod wires these itself (the overlay source is the pod's alone).
POD_OWNED = ("custom_overlay_src", "custom_overlay_src_alternate")
# Region intent: carried in the payload from the spec, never overridable.
SPEC_OWNED = ("skip_converts",)


def set_app_var(CFG, key, value):
    """Set an app-level variable on the module that owns it, per
    cfg_app_vars[key]["module"]. Modules are imported only when targeted."""
    binding = CFG.cfg_app_vars[key].get("module")
    if binding not in APP_MODULES:
        raise KeyError("app variable %r has no owning module (binding %r)" % (key, binding))
    import importlib
    setattr(importlib.import_module(APP_MODULES[binding]), key, value)


def apply_app_level(CFG, config, overrides, raw):
    """skip_converts from the payload, then the pod's overrides. Everything
    is validated and converted per the variable's declared type."""
    # Refused for every task type: only ortho builds a Tile from raw, but an
    # app-level key in any task's raw is a silent no-op worth failing loudly.
    for k in raw:
        if k in CFG.cfg_app_vars:
            raise ValueError(
                "%r is an app-level variable; app-level keys belong in the pod's overrides, not raw" % k
            )
    if not isinstance(config.get("skip_converts"), bool):
        raise ValueError("config.skip_converts must be a bool, got %r" % (config.get("skip_converts"),))
    set_app_var(CFG, "skip_converts", config["skip_converts"])
    for k, v in overrides.items():
        if k in POD_OWNED:
            raise ValueError("app variable %r is wired by the pod and cannot be overridden" % k)
        if k in SPEC_OWNED:
            raise ValueError(
                "app variable %r is region intent and belongs in the region spec, not the pod's overrides" % k
            )
        if k not in CFG.cfg_app_vars:
            if k in CFG.cfg_vars:
                raise KeyError(
                    "%r is a tile-level variable, not an app-level one; it belongs in the spec's raw" % k
                )
            raise KeyError("unknown app-level variable %r in overrides" % k)
        set_app_var(CFG, k, convert_raw(CFG.cfg_vars, k, v))


def check(fn_name, result, none_is_success=False):
    """Ortho4XP's build functions return 0 on an internally handled failure
    and exit 0 regardless, so the return value is the only failure signal.
    Convention, per the pinned source: build_poly_file, build_mesh,
    build_tile and build_overlay end in `return 1` (every failure path is
    `return 0`). build_masks ends in a bare `return` (None) on success and
    uses `return 0` only on its failure paths, so None must count as success
    there or every real tile would fail."""
    if result is None and none_is_success:
        return
    if not result:
        raise BuildFailed("%s returned %r" % (fn_name, result))


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

        # Both task types read X-Plane's Global Scenery. The variable lives in
        # O4_Overlay_Utils (O4_Cfg_Vars binds it to OVL), not in CFG.
        OVL.custom_overlay_src = inp["overlay_src"]

        # App-level variables go to their owning modules before anything
        # runs (the providers read some of them).
        phase = "configure"
        apply_app_level(CFG, config, inp.get("app_overrides") or {}, config.get("raw") or {})
        sys.stderr.write(
            "patches: %s for %s\n"
            % ("present" if os.path.isdir(FNAMES.patch_dir(lat, lon)) else "none", inp["tile"])
        )
        sys.stderr.flush()

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
                setattr(tile, k, convert_raw(CFG.cfg_vars, k, v))
            for fn in (VMAP.build_poly_file, MESH.build_mesh, MASK.build_masks, TILE.build_tile):
                phase = fn.__name__
                check(phase, fn(tile), none_is_success=(fn is MASK.build_masks))
        else:
            phase = "prepare_overlay_dir"
            os.makedirs(
                os.path.join(FNAMES.Overlay_dir, "Earth nav data", FNAMES.round_latlon(lat, lon)),
                exist_ok=True,
            )
            phase = "build_overlay"
            check(phase, OVL.build_overlay(lat, lon))
    except BaseException as e:  # SystemExit from O4 included
        reason = str(e) if isinstance(e, BuildFailed) else "%s: %s" % (type(e).__name__, e)
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
