- A budget stop raised when a finished call settles past the ceiling now names
  the spend the ceiling judged in its `budget_exhausted` event. It used to
  carry no session spend, so a report could say `$0.0000` for a stop that had
  spent past the cap.
