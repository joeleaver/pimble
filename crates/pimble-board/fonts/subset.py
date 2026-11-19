"""How the bundled faces beside this file were made (fonttools 4.68).

    python3 -m venv env && env/bin/pip install fonttools
    env/bin/python subset.py <dir with NotoSans-VF.ttf> <out dir>

NotoSans-VF.ttf is google/fonts ofl/notosans/NotoSans[wdth,wght].ttf; the
Sans bold and italic and Serif faces are Debian's fonts-noto-core
(20201225, Noto 2.004). NotoSans-Regular.ttf is the full, unmodified font
and is not made here. See OFL.txt.
"""
import sys

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

src, out = sys.argv[1], sys.argv[2]
SYSDIR = "/usr/share/fonts/truetype/noto"

# Noto Sans Condensed: the variable font at width 75, regular and bold.
for style, wght in [("Regular", 400), ("Bold", 700)]:
    f = instancer.instantiateVariableFont(TTFont(f"{src}/NotoSans-VF.ttf"), {"wdth": 75, "wght": wght}, updateFontNames=False)
    names = f["name"]
    names.names = [r for r in names.names if r.platformID == 3]
    names.setName("Noto Sans Condensed", 1, 3, 1, 0x409)
    names.setName(style, 2, 3, 1, 0x409)
    names.setName(f"Noto Sans Condensed {style}", 4, 3, 1, 0x409)
    names.setName(f"NotoSansCondensed-{style}", 6, 3, 1, 0x409)
    f.save(f"{out}/NotoSansCondensed-{style}.full.ttf")

# Latin (Basic, Latin-1, Extended A and B, IPA, spacing modifiers, combining
# marks, Extended Additional), punctuation, super- and subscripts, currency,
# letterlike symbols, number forms, arrows, a few math signs, ligatures.
UNICODES = (
    "U+0020-007E,U+00A0-024F,U+0250-02FF,U+0300-036F,U+1E00-1EFF,U+2000-206F,U+2070-209F,U+20A0-20CF,"
    "U+2100-214F,U+2150-218F,U+2190-21FF,U+2212,U+2215,U+221E,U+2248,U+2260,U+2264,U+2265,U+25CB,U+25CF,"
    "U+FB00-FB06,U+FFFD"
)
FACES = [(f"{SYSDIR}/NotoSans-{s}.ttf", f"NotoSans-{s}") for s in ("Bold", "Italic", "BoldItalic")]
FACES += [(f"{SYSDIR}/NotoSerif-{s}.ttf", f"NotoSerif-{s}") for s in ("Regular", "Bold", "Italic", "BoldItalic")]
FACES += [(f"{out}/NotoSansCondensed-{s}.full.ttf", f"NotoSansCondensed-{s}") for s in ("Regular", "Bold")]

for path, name in FACES:
    opts = subset.Options()
    # Pimble does not shape or kern: the layout tables and hinting go.
    opts.layout_features = []
    opts.hinting = False
    opts.name_IDs = [0, 1, 2, 3, 4, 5, 6, 13, 14]
    opts.name_languages = ["*"]
    opts.notdef_outline = True
    opts.glyph_names = False
    opts.drop_tables += ["DSIG", "GPOS", "GSUB", "GDEF", "kern", "STAT", "MVAR", "HVAR"]
    f = TTFont(path)
    s = subset.Subsetter(opts)
    s.populate(unicodes=subset.parse_unicodes(UNICODES))
    s.subset(f)
    f.save(f"{out}/{name}.ttf")
    print(name, len(f.getGlyphOrder()), "glyphs")
