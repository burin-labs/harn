Keep CLI bootstrap parsing on Harn's explicitly sized runtime thread so the
growing command surface does not overflow the default Windows process stack.
