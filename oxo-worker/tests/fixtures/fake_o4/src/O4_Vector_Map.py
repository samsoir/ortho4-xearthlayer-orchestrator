import json
from fake_log import enter, rec
import O4_Overlay_Utils as OVL


def build_poly_file(tile):
    enter("build_poly_file")
    rec("cfg overlay_src=" + OVL.custom_overlay_src)
    rec("tileattrs " + json.dumps({k: v for k, v in vars(tile).items() if k not in ("lat", "lon")}, sort_keys=True))
