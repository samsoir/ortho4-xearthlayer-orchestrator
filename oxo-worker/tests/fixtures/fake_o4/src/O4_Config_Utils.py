from fake_log import rec

# Minimal slices of O4_Cfg_Vars: only "type" (and, for app-level variables,
# the owning "module" alias) matter to the runner.
cfg_app_vars = {
    "skip_converts": {"module": "TILE", "type": bool},
    "max_download_slots": {"module": "TILE", "type": int},
    "http_timeout": {"module": "IMG", "type": float},
    "ovl_exclude_pol": {"module": "OVL", "type": list},
    "custom_overlay_src": {"module": "OVL", "type": str},
    "custom_overlay_src_alternate": {"module": "OVL", "type": str},
}

cfg_tile_vars = {
    "cover_zl": {"type": int},
    "clean_bad_geometries": {"type": bool},
    "sea_texture_blur": {"type": float},
    "zone_list_like": {"type": list},
}

# As in the real O4_Cfg_Vars: app-level variables are part of cfg_vars too.
cfg_vars = {**cfg_app_vars, **cfg_tile_vars}


class Tile:
    def __init__(self, lat, lon, custom_build_dir):
        self.lat, self.lon = lat, lon
        rec("tile %d %d %r" % (lat, lon, custom_build_dir))
