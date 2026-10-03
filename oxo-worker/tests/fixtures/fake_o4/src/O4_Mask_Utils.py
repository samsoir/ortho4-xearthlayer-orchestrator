import json
from fake_log import enter, rec
import O4_Config_Utils as CFG


def build_masks(tile):
    enter("build_masks")
    rec("cfg overlay_src=" + CFG.custom_overlay_src)
    rec("tileattrs " + json.dumps({k: v for k, v in vars(tile).items() if k not in ("lat", "lon")}, sort_keys=True))
