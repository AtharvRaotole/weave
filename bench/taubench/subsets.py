"""Frozen tau-bench task subsets. `retail_test7` is pre-registered in Addendum 5:
sorted(random.Random("weave-round5-tau-retail-test").sample(range(115), 7)) over tau-bench's retail test split."""

SUBSETS = {
    "retail_test7": ("test", [18, 22, 45, 52, 56, 76, 105]),
    # Addendum 6: sorted(random.Random("weave-round6-tau-retail-test").sample(
    #     [i for i in range(115) if i not in retail_test7], 10))
    "retail_test10r6": ("test", [39, 42, 49, 53, 60, 79, 85, 89, 96, 112]),
    # Addendum 7: sorted(random.Random("weave-round7-tau-retail-test").sample(
    #     [i for i in range(115) if i not in retail_test7 + retail_test10r6], 12))
    "retail_test12r7": ("test", [0, 2, 7, 9, 50, 54, 70, 72, 73, 83, 94, 101]),
}
