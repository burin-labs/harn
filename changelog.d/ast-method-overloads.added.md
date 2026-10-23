Added `harness.ast.check_member_insertion` to distinguish Java method overloads
from duplicate signatures before writing a proposed edit. The check compares
direct members of one type, ignores parameter names and return types, and
refuses unresolved cases.
