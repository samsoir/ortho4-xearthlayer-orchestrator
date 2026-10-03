import json
from fake_log import enter, rec, ret
import O4_Overlay_Utils as OVL


def build_tile(tile):
    enter("build_tile")
    rec("cfg overlay_src=" + OVL.custom_overlay_src)
    rec("tileattrs " + json.dumps({k: v for k, v in vars(tile).items() if k not in ("lat", "lon")}, sort_keys=True))
    return ret("build_tile")
