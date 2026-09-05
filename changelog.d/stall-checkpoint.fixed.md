Agent loops can carry an explicit session-bound stall checkpoint across workflow stages without losing final verification
or counting dispatch twice. Verification uses the completion evidence role: successful reads and deferred calls cannot
clear failures, and the last executed verification wins.
