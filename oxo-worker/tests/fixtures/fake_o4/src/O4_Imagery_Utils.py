import os
from fake_log import rec


def initialize_extents_dict(): rec("img initialize_extents_dict")
def initialize_color_filters_dict(): rec("img initialize_color_filters_dict")
def initialize_providers_dict():
    rec("img initialize_providers_dict")
    if os.environ.get("FAKE_O4_FAIL") == "initialize_providers_dict":
        raise RuntimeError("boom in initialize_providers_dict")
def initialize_combined_providers_dict(): rec("img initialize_combined_providers_dict")
