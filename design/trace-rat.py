#!/usr/bin/env python3
"""Trace a solid black-on-white silhouette PNG into an SVG path."""
import sys
import numpy as np
from PIL import Image

# clockwise neighbour offsets in (row, col): N,NE,E,SE,S,SW,W,NW
CW = [(-1,0),(-1,1),(0,1),(1,1),(1,0),(1,-1),(0,-1),(-1,-1)]

def moore(mask):
    m = np.pad(mask, 1, constant_values=False)
    rows, cols = np.where(m)
    top = rows.min()
    left = cols[rows == top].min()
    start = (int(top), int(left))
    bt0 = (start[0], start[1]-1)          # backtracker: west of start
    contour = []
    b, p = start, bt0
    while True:
        contour.append(b)
        po = (p[0]-b[0], p[1]-b[1])     # offset from b to backtracker p
        idx = CW.index(po)                 # start cw scan AFTER p
        nxt = None
        for k in range(1, 9):
            off = CW[(idx+k) % 8]
            if m[b[0]+off[0], b[1]+off[1]]:
                prev = CW[(idx+k-1) % 8]     # last neighbour before the hit
                nxt = (b[0]+off[0], b[1]+off[1])
                new_p = (b[0]+prev[0], b[1]+prev[1])
                break
        if nxt is None:
            break                       # isolated pixel
        b, p = nxt, new_p
        if b == start and p == bt0:      # Jacob stop: back to start, same backtracker
            break
    return contour

def dp(points, eps):
    if len(points) < 3:
        return points[:]
    def rec(pts):
        if len(pts) < 3:
            return pts
        a, b = pts[0], pts[-1]
        dmax, idx = 0, -1
        for i in range(1, len(pts)-1):
            d = abs(perp(pts[i], a, b))
            if d > dmax:
                dmax, idx = d, i
        if dmax > eps:
            return rec(pts[:idx+1]) + rec(pts[idx:])[1:]
        return [a, b]
    return rec(points)

def perp(p, a, b):
    return (b[0]-a[0])*(p[1]-a[1]) - (b[1]-a[1])*(p[0]-a[0])

def main():
    im = Image.open(sys.argv[1]).convert('L')
    mask = np.array(im) < 128
    contour = moore(mask)
    pts = dp(contour, float(sys.argv[2]) if len(sys.argv) > 2 else 1.5)
    # bounding box
    ys = [p[0] for p in pts]; xs = [p[1] for p in pts]
    y0, y1 = min(ys), max(ys); x0, x1 = min(xs), max(xs)
    sw, sh = x1-x0, y1-y0
    side = max(sw, sh)
    # scale to 128, center
    S = 128.0
    scale = S / side
    ox = (S - sw*scale) / 2.0
    oy = (S - sh*scale) / 2.0
    def tx(p):
        return (p[1]-x0)*scale + ox, (p[0]-y0)*scale + oy
    out = [tx(p) for p in pts]
    d = 'M %.2f %.2f ' % out[0]
    for p in out[1:]:
        d += 'L %.2f %.2f ' % p
    d += 'Z'
    print('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" width="128" height="128">')
    print('<path fill="#8b1e3f" d="%s"/>' % d)
    print('</svg>')

if __name__ == '__main__':
    main()
