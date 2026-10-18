#!/usr/bin/env python3
"""Original landscape-frame icon, MIT OR Apache-2.0; no third-party artwork."""
from pathlib import Path
import hvif
icon = {'styles': [{'color': hvif.hex_color(c)} for c in ['#2F7BF5', '#EFE9DC', '#0B253E', '#E9AE38']], 'paths': [], 'shapes': []}
def shape(style, points):
    n = len(icon['paths'])
    icon['paths'].append({'closed': True, 'points': points})
    icon['shapes'].append({'style': style, 'paths': [n]})
shape(0, [(4,4),(60,4),(60,60),(4,60)])
shape(1, [(10,12),(54,12),(54,52),(10,52)])
shape(0, [(14,16),(50,16),(50,48),(14,48)])
shape(2, [(14,44),(25,29),(34,40),(41,32),(50,43),(50,48),(14,48)])
shape(3, [(38,20),(44,20),(44,26),(38,26)])
out = Path(__file__).resolve().parents[2] / 'apps/photocraft-haiku/resources/PhotoCraft.hvif'
out.write_bytes(hvif.encode(icon))
