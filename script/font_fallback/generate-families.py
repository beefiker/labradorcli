'''
Generates the `ExternalFontFamily` definitions used in `app/src/font_fallback.rs`.
These definitions contain the URLs to each external fallback font we use in Labrador.
Generated code is sent to stdout.

This script will read our cloud storage bucket to retrieve the names of the fonts
we support, and generate the code required to initialize static references for
each font family.

Assumes that the fallback fonts in the prod and staging static asset buckets are
identical.

Usage:
1. Make sure the gcloud CLI is installed and you are authed via `gcloud auth login`.
2. Run `python3 generate_families.py`
3. Set `LABRADOR_STATIC_ASSETS_BUCKET` to override the default static assets bucket.
4. Manually inspect the name for each font. The script will generate the name in
   title-case, but this isn't correct for some fonts (e.g. Noto Sans SC).
'''

import os
import subprocess
from collections import defaultdict

STATIC_ASSETS_BUCKET_ENV = "LABRADOR_STATIC_ASSETS_BUCKET"
DEFAULT_STATIC_ASSETS_BUCKET = "labrador-static-assets"


def candidate_fallback_font_prefixes():
    if bucket := os.environ.get(STATIC_ASSETS_BUCKET_ENV):
        return [f"gs://{bucket}/fallback-fonts/"]

    return [
        f"gs://{DEFAULT_STATIC_ASSETS_BUCKET}/fallback-fonts/",
    ]


def list_fonts():
    for uri_prefix in candidate_fallback_font_prefixes():
        command = f"gcloud storage ls --recursive '{uri_prefix}**.ttf'"
        try:
            return uri_prefix, subprocess.check_output(command, shell=True, text=True).splitlines()
        except subprocess.CalledProcessError:
            if os.environ.get(STATIC_ASSETS_BUCKET_ENV):
                raise

    raise RuntimeError("Failed to list fallback fonts from the Labrador static assets bucket")


def generate_families(uri_prefix, font_uris):
    family_map = defaultdict(list)
    for uri in font_uris:
        parts = uri.removeprefix(uri_prefix).split('/')
        family_name = parts[0]
        font_name = parts[1]
        family_map[family_name].append(font_name)

    for family_name, font_names in family_map.items():
        print_family(family_name, font_names)


def indent_level(level, s):
    indent = "    " * level
    return indent + s


def print_family(family_name, font_names):
    variable_name = family_name.replace('-', '_').upper()
    title_case_name = family_name.replace('-', ' ').title()

    print(f"static ref {variable_name}: ExternalFontFamily = ExternalFontFamily {{")
    # Title-case is not correct for some fonts, e.g. "Noto Sans SC", so we add
    # a todo to make any manual adjustments.
    print(indent_level(1, f"name: \"{title_case_name}\", // TODO: double-check the title is correct"))
    print(indent_level(1, "font_urls: Arc::new(vec!["))
    for font_name in font_names:
        print(indent_level(2, f"url_for_font(\"{family_name}\", \"{font_name}\"),"))
    print(indent_level(1, "]),"))
    print("};")


def main():
    uri_prefix, font_uris = list_fonts()
    generate_families(uri_prefix, font_uris)


if __name__ == "__main__":
    main()
