def report(base, qty):
    subtotal = base * qty
    audit(subtotal)
    label = base + qty
    audit(label)
