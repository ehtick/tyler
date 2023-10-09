"""
Copyright 2023 Balázs Dukai, Ravi Peters

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

   http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
"""
import json
import argparse
from pathlib import Path

parser=argparse.ArgumentParser(description="Multiply geometric error values by factor. Make sure to back up tileset json files before running.")
parser.add_argument("input_tileset", help="Main tileset input file (.json)")
parser.add_argument("factor", type=float, help="Multiply geometric error values by this factor")
parser.add_argument("--overwrite", action='store_true', help="Overwrite files without warning")

def adjust_node(node, factor, external_tilesets):
    node["geometricError"] = node["geometricError"] * factor

    if node.get("content"):
        if node["content"].get("uri"):
            if node["content"]["uri"].endswith(".json"):
                external_tilesets.append(node["content"]["uri"])

    if node.get("children"):
        for child in node["children"]:
            adjust_node(child, factor, external_tilesets)

def adjust_tileset(tileset_json, factor, overwrite):
    external_tilesets = []
    with open(tileset_json, "r") as in_file:
        tileset = json.load(in_file)
    
        # find and modify geometric error values
        tileset["geometricError"] = tileset["geometricError"] * factor
        adjust_node(tileset["root"], factor, external_tilesets)

        # save json
        if (tileset_json).exists() and not overwrite:
            overwrite = input(f'Tileset file \'{tileset_json}\' already exists. Overwrite and loose original contents? Y = yes, N = no\n')
            if not overwrite.lower() == 'y': exit()

        with open(tileset_json, "w") as file:
            json.dump(tileset, file)

    # adjust external tilesets if any
    for ext_tileset_json in external_tilesets:
        adjust_tileset(Path(ext_tileset_json), factor, overwrite)

if __name__ == "__main__":
    args = parser.parse_args()

    adjust_tileset(Path(args.input_tileset), args.factor, args.overwrite)
