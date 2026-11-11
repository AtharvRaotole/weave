"""Frozen tau-bench task subsets. `retail_test7` is pre-registered in Addendum 5:
sorted(random.Random("weave-round5-tau-retail-test").sample(range(115), 7)) over tau-bench's retail test split."""

SUBSETS = {"retail_test7": ("test", [18, 22, 45, 52, 56, 76, 105])}
