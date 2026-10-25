class Ledger:
    def __init__(self, balances):
        self._balances = dict(balances)
        self._receipts = {}

    def balances(self):
        return dict(self._balances)

    def transfer(self, operation_id, source, destination, amount):
        if type(amount) is not int or amount <= 0 or source == destination:
            raise ValueError("invalid transfer")
        arguments = (source, destination, amount)
        if operation_id in self._receipts:
            previous, receipt = self._receipts[operation_id]
            if arguments != previous:
                raise ValueError("id conflict")
            return receipt
        if source not in self._balances or destination not in self._balances or self._balances[source] < amount:
            raise ValueError("invalid transfer")
        self._balances[source] -= amount
        self._balances[destination] += amount
        receipt = (operation_id, self._balances[source], self._balances[destination])
        self._receipts[operation_id] = (arguments, receipt)
        return receipt

    def batch(self, operations):
        balances, receipts = self._balances.copy(), self._receipts.copy()
        try:
            return [self.transfer(*operation) for operation in operations]
        except Exception:
            self._balances, self._receipts = balances, receipts
            raise
