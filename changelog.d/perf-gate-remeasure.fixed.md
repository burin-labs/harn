The parallel test-case performance gate re-measures on a breach (up to 3
rounds) and judges the best observation per metric, so a transient noisy
neighbor on a shared CI runner no longer fails the gate while a real
regression, which breaches every round, still does.
