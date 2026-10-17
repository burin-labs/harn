Smart Ship rejects non-finite repair budgets and integer budgets that lose precision
when represented as floating-point values. Both trusted and submitted policies are
validated before matching admission provenance, preventing a changed integer budget
from retaining another budget's signature through a numeric collision.
