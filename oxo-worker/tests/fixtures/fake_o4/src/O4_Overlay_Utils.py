import os
from fake_log import enter, rec
import O4_File_Names as FNAMES
import O4_Config_Utils as CFG


def build_overlay(lat, lon):
    enter("build_overlay")
    d = os.path.join(FNAMES.Overlay_dir, "Earth nav data", FNAMES.round_latlon(lat, lon))
    rec("blockdir_exists %s" % os.path.isdir(d))
    rec("cfg overlay_src=" + CFG.custom_overlay_src)
    rec("args %d %d" % (lat, lon))
