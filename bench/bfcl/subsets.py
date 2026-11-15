"""Frozen BFCL multi_turn_base subsets (ids are `multi_turn_base_<i>`, i in 0..199).

`dev6` is for tuning the Weave arm and is never evidence:
sorted(random.Random("weave-round8-bfcl-dev").sample(range(200), 6))."""

import random

DEV = sorted(random.Random("weave-round8-bfcl-dev").sample(range(200), 6))

# Addendum 8 (evidence): sorted(random.Random("weave-round8-bfcl-evidence").sample(
#     [i for i in range(200) if i not in DEV], 40))
EVIDENCE = [13, 14, 17, 21, 28, 30, 36, 41, 43, 45, 52, 57, 61, 70, 80, 87, 88, 93, 95, 96, 102, 103, 108, 111, 116,
            120, 126, 136, 140, 161, 163, 177, 181, 182, 187, 188, 192, 193, 194, 199]

SUBSETS = {
    "dev6": ("dev", DEV),
    "mtb40": ("evidence", EVIDENCE),
}
