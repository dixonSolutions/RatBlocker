# Design assets

`design/reference-rat.png` is the **authoritative mark**: a clean, whisker-less
rat silhouette in profile, facing right. `design/icon.svg` and `design/banner.svg`
are both *derived* from it — edit the reference image (or the tracer), re-run the
commands below, and commit the regenerated SVG and PNGs. Do not hand-edit the
SVG paths or touch the PNGs by hand.

## The mark

A single rat silhouette in the brand accent `#8b1e3f` (the same accent used by
`extensions/shared/ui/ui.css`), facing right, in a crouched posture with one
ear, a pointed snout, a long curved tail, and a small front paw. It is a pure
silhouette — no eyes, no whiskers, no shield — which is what makes it read at
16px: the whole shape is the silhouette, and there are no fine details to lose.

The previous mark wrapped a rat in a shield and gave it detached whiskers;
both read poorly at small sizes (the shield left no room for the rat, and the
whiskers floated off the nose). The new mark drops the shield and the
whiskers entirely for a cleaner shape that scales from 16px to 512px.

`banner.svg` does not copy the rat — it reuses the one definition from
`icon.svg` (`<use href="icon.svg#rat">` with `color="#ffffff"`), so the
icon and the banner can never drift apart. The banner inverts the mark to white
on its own dark field (`#2a0f1c`) so it reads the same in a light or a dark
README.

## Rendering

The rat is traced from the reference PNG with [`rsvg-convert`](https://gitlab.gnome.org/GNOME/librsvg)
and `design/trace-rat.py` (Moore-neighbor boundary trace + Douglas-Peucker
simplification), then wrapped with `design/assemble-icon.py`. Run from the
repository root:

```sh
# Re-trace the rat from the reference image, then wrap it as icon.svg.
PYTHONPATH=/usr/lib/python3/dist-packages \
  python3 design/trace-rat.py design/reference-rat.png 3.0 > /tmp/rat-path.txt
python3 design/assemble-icon.py /tmp/rat-path.txt > design/icon.svg

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

# Banner. Reuses the rat from icon.svg in white on the dark field.
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
