# Design assets

`icon.svg` and `banner.svg` are the **authoritative sources**. Every PNG in this
directory and in `extensions/shared/icons/` is generated from them — edit the
SVG, re-run the commands below, and commit both the SVG and the regenerated
PNGs. Do not touch the PNGs by hand.

## The mark

A shield holding a rat's head in profile, in the brand accent `#8b1e3f` (the
same accent used by `extensions/shared/ui/ui.css`) with a white knockout.

The two elements carry different weights on purpose. Only the shield has to
survive at 16px, where it supplies the whole silhouette in a browser toolbar;
the head is allowed to resolve from 32px upwards, and the head is scaled to
0.64 of the shield's safe area so a band of accent survives on every side at
that size.

The head is one closed outline, not a union of primitives. Unions were tried
first and kept collapsing: the ear merged into the skull and the muzzle read as
a flag, leaving an anonymous teardrop. The whiskers matter more than they look
— a tapered muzzle beside a round ear reads just as readily as a bird, and
three strokes leaving the snout are what settle it as a rodent. They fall below
a pixel at 16px and disappear, which is the intended degradation.

`banner.svg` reuses those shapes verbatim, inverted (white shield, plum rat), on
a dark field of its own so the banner reads the same in a light or a dark
README.

## Rendering

Rendered with [`rsvg-convert`](https://gitlab.gnome.org/GNOME/librsvg) (Debian
and Ubuntu: `librsvg2-bin`). Run from the repository root:

```sh
# Extension icons. These paths and filenames are referenced by both browser
# manifests, so they must not change.
for size in 16 32 48 128; do
    rsvg-convert -w "$size" -h "$size" design/icon.svg \
        -o "extensions/shared/icons/icon-$size.png"
done

# General-purpose sizes.
for size in 256 512; do
    rsvg-convert -w "$size" -h "$size" design/icon.svg -o "design/icon-$size.png"
done

# Banner.
rsvg-convert -w 1280 design/banner.svg -o design/banner.png

# AMO listing preview. AMO prefers 1280x800 screenshots; the banner is
# 1280x320, so it is centred on the banner's own dark background to make a
# preview that fills AMO's screenshot box without distortion.
magick -size 1280x800 "xc:$(magick identify -format '%[pixel:p{2,2}]' design/banner.png)" \
    -gravity center design/banner.png -composite design/preview-1280x800.png
```

`banner.svg` sets type in a plain sans-serif stack rather than embedding
outlines, so a machine without Lato installed will render the banner in
whatever the next fallback is. The checked-in `banner.png` is the reference.
