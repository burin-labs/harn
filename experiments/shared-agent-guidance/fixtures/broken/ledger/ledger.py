class Ledger:
    def __init__(self, balances):
        self._balances = balances
        self._receipts = {}

    def balances(self):
        return self._balances

    def transfer(self, operation_id, source, destination, amount):
        if operation_id in self._receipts:
            return self._receipts[operation_id]
        if amount < 0 or self._balances[source] < amount:
            raise ValueError("invalid transfer")
        self._balances[source] -= amount
        self._balances[destination] += amount
        receipt = (operation_id, self._balances[source], self._balances[destination])
        self._receipts[operation_id] = receipt
        return receipt

    def batch(self, operations):
        return [self.transfer(*operation) for operation in operations]
