import os

Provider_dir = os.path.join(os.getcwd(), "Providers")
Overlay_dir = os.path.join(os.getcwd(), "yOrtho4XP_Overlays")


def round_latlon(lat, lon):
    from math import floor
    return "{:+.0f}".format(floor(lat / 10) * 10).zfill(3) + "{:+.0f}".format(floor(lon / 10) * 10).zfill(4)
