from fake_log import rec

custom_overlay_src = ""


class Tile:
    def __init__(self, lat, lon, custom_build_dir):
        self.lat, self.lon = lat, lon
        rec("tile %d %d %r" % (lat, lon, custom_build_dir))
