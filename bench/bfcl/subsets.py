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

# Round 9. Evidence: 60 tasks never used before (not dev6, not mtb40). Everything else is train: the Weave arm may be
# tuned on it, and return shapes are learned from its ground-truth executions (bench/bfcl/learn_shapes.py).
R9_POOL = [i for i in range(200) if i not in DEV and i not in EVIDENCE]
R9_EVIDENCE = sorted(random.Random("weave-round9-bfcl-evidence").sample(R9_POOL, 60))
TRAIN = [i for i in range(200) if i not in R9_EVIDENCE]
# Ablations run on 20 train tasks drawn from mtb40, whose round-8 native rows give a reference.
R9_DEV = sorted(random.Random("weave-round9-bfcl-dev").sample(EVIDENCE, 20))

SUBSETS["dev20r9"] = ("dev", R9_DEV)
SUBSETS["mtb60r9"] = ("evidence", R9_EVIDENCE)
# A second ablation set: 20 train tasks outside mtb40 and dev6 (for decisions on 40 dev tasks, not 20).
R9_DEV_B = sorted(random.Random("weave-round9-bfcl-dev-b").sample(
    [i for i in TRAIN if i not in EVIDENCE and i not in DEV], 20))
SUBSETS["dev40r9"] = ("dev", sorted(R9_DEV + R9_DEV_B))
SUBSETS["devB20r9"] = ("dev", R9_DEV_B)

# Round 10 (other multi-turn categories; ids index the same 200 base scenarios). Dev: train ids only.
R10_DEV_MP = sorted(random.Random("weave-round10-bfcl-dev-mp").sample([i for i in TRAIN if i not in DEV], 20))
R10_DEV_MF = sorted(random.Random("weave-round10-bfcl-dev-mf").sample([i for i in TRAIN if i not in DEV], 10))
SUBSETS["devMP20r10"] = ("dev", R10_DEV_MP)
SUBSETS["devMF10r10"] = ("dev", R10_DEV_MF)
