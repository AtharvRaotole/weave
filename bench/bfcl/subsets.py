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
# Round 10 evidence: the 60 scenario ids no round tuned on (R9_EVIDENCE: not train, so no shapes or effects were
# learned from them), in two categories never run before; GPT-4.1 on a seeded 20 of them (budget).
R10_EVIDENCE = list(R9_EVIDENCE)
R10_EVIDENCE_41 = sorted(random.Random("weave-round10-gpt41").sample(R9_EVIDENCE, 20))
SUBSETS["mp60r10"] = ("evidence", R10_EVIDENCE)
SUBSETS["mf60r10"] = ("evidence", R10_EVIDENCE)
SUBSETS["mp20r10"] = ("evidence", R10_EVIDENCE_41)

# Round 11 (Azure). Completion of the round-10 pairs a provider credit outage (or, for GPT-4.1, the budget) cut off;
# both arms are rerun for each incomplete pair.
SUBSETS["mp60r10c"] = ("evidence", [31, 58, 72, 113, 185])
SUBSETS["mf60r10c"] = ("evidence", [23, 31, 58, 72, 86, 113, 130, 141, 147, 185])
SUBSETS["mp20r10c"] = ("evidence", [32, 117, 157, 175])
# Confirmation on scenario ids never run live in any category or round: not dev6, mtb40, dev40r9 (incl. devB20r9),
# mtb60r9/R10 evidence, devMP20r10 or devMF10r10. They are train ids, so round 9's learned return shapes and round
# 10's learned read/write classes include executions of their ground truth (API-level metadata, no model runs).
_USED = set(DEV) | set(EVIDENCE) | set(R9_DEV_B) | set(R9_EVIDENCE) | set(R10_DEV_MP) | set(R10_DEV_MF)
R11_FRESH = [i for i in range(200) if i not in _USED]
for _cat in ("mtb", "mp", "mf"):
    SUBSETS[f"{_cat}fresh{len(R11_FRESH)}r11"] = ("evidence", R11_FRESH)
SUBSETS["smoke2"] = ("dev", [5, 6])  # Azure endpoint smoke test (train ids, not evidence)

# Round 15 (docs/research/round15-preregistration.md). Primary: multi_turn_long_context, never run in any round, on
# 100 scenario ids outside R9_EVIDENCE (the 60 read for the round 15 diagnosis). These are all train ids of the base
# category (learned shapes and read/write classes include their base ground truth); 53 were run live before in
# other categories. Secondary (contaminated): mtb60r9, unchanged.
R15_LC = sorted(random.Random("weave-round15-bfcl-heldout-lc").sample([i for i in range(200) if i not in R9_EVIDENCE], 100))
SUBSETS["lc100r15"] = ("evidence", R15_LC)
R15_LC_NEVER_RUN = [i for i in R15_LC if i not in _USED]  # 47 ids: reported descriptively, not a new endpoint
# Round 15 cost smoke (dev split, not evidence): one long-context id outside lc100r15 and R9_EVIDENCE.
SUBSETS["smokelc15"] = ("dev", [[i for i in range(200) if i not in R15_LC and i not in R9_EVIDENCE][0]])
# Round 15 deviation 6: completing run A. Both arms of the 11 lc100r15 tasks run A had not finished when it was
# stopped (Azure stalls), same seed rules, with the 120 s timeout.
SUBSETS["lc11r15a"] = ("evidence", [20, 24, 52, 87, 109, 132, 133, 140, 149, 155, 159])

# Round 16 (docs/research/round16-preregistration.md): confirmation on (category, id) pairs no earlier round ran live.
# Every scenario id was run live in some category before, so freshness is per category: the long-context ids not in
# lc100r15, not in R9_EVIDENCE (read for the round 15 diagnosis) and not the round 15 smoke id 10; and the
# miss_param / miss_func ids that no results file in bench/results holds a row for (any model).
_R16_NO_LC = set(R15_LC) | set(R9_EVIDENCE) | {10}
R16_LC = [i for i in range(200) if i not in _R16_NO_LC]
R16_MP = [4, 5, 6, 13, 14, 17, 20, 21, 24, 25, 28, 39, 41, 43, 45, 46, 49, 57, 61, 67, 70, 73, 80, 87, 88, 93, 95, 96, 100, 102, 103, 105, 107, 108, 111, 114, 116, 118, 119, 122, 126, 135, 136, 140, 145, 158, 159, 163, 177, 181, 182, 187, 188, 192, 196, 198, 199]
R16_MF = [4, 6, 8, 13, 14, 17, 18, 20, 21, 25, 28, 29, 30, 34, 36, 37, 39, 41, 45, 46, 49, 52, 54, 57, 61, 67, 70, 73, 80, 87, 88, 93, 95, 100, 102, 103, 108, 111, 116, 118, 119, 120, 121, 122, 126, 131, 133, 135, 136, 139, 140, 143, 145, 151, 158, 161, 163, 173, 181, 182, 187, 188, 192, 194, 196, 198, 199]
SUBSETS["lc39r16"] = ("evidence", R16_LC)
SUBSETS["mp57r16"] = ("evidence", R16_MP)
SUBSETS["mf67r16"] = ("evidence", R16_MF)
