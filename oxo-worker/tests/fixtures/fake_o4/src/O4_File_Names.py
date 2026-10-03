import os

Provider_dir = os.path.join(os.getcwd(), "Providers")
Overlay_dir = os.path.join(os.getcwd(), "yOrtho4XP_Overlays")


def round_latlon(lat, lon):
    from math import floor
    return "{:+.0f}".format(floor(lat / 10) * 10).zfill(3) + "{:+.0f}".format(floor(lon / 10) * 10).zfill(4)


Patch_dir = os.path.join(os.getcwd(), "Patches")


def long_latlon(lat, lon):
    return os.path.join(round_latlon(lat, lon), "{:+.0f}".format(lat).zfill(3) + "{:+.0f}".format(lon).zfill(4))


def patch_dir(lat, lon):
    return os.path.join(Patch_dir, long_latlon(lat, lon))
