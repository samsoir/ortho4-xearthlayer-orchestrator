from fake_log import rec

# Minimal slice of O4_Cfg_Vars.cfg_vars: only "type" matters to the runner.
cfg_vars = {
    "cover_zl": {"type": int},
    "clean_bad_geometries": {"type": bool},
    "sea_texture_blur": {"type": float},
    "zone_list_like": {"type": list},
}


class Tile:
    def __init__(self, lat, lon, custom_build_dir):
        self.lat, self.lon = lat, lon
        rec("tile %d %d %r" % (lat, lon, custom_build_dir))
