#!/usr/bin/env python3
"""Assemble design/icon.svg from the traced path + a header."""
import sys

HEADER = '''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" width="128" height="128"
     role="img" aria-label="RatBlocker">
  <title>RatBlocker</title>
  <!--
    Authoritative source for every RatBlocker icon. Auto-traced from
    design/reference-rat.png with design/trace-rat.py (Moore-neighbor
    boundary trace + Douglas-Peucker simplification). To re-derive, edit the
    reference image (or the tracer) and re-run the render command in
    design/README.md. The mark is a single rat silhouette in the brand
    accent #8b1e3f, facing right; it carries at every size, and the shield
    the old icon wrapped it in has been dropped for a cleaner mark.
  -->
'''
FOOTER = '</svg>\n'

path = open(sys.argv[1]).read().strip()
sys.stdout.write(HEADER + path + '\n' + FOOTER)
