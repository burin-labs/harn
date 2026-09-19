- **A refusal the automated reviewer answered is no longer credited to a
  person.** When a reviewer refused a call, the refusal still travelled on to
  the host, and a host that returns no decision metadata defaults the recorded
  decider to `person`. In a non-interactive run that credited a decision nobody
  made, on a call the reviewer had already settled, while the same record
  carried the reviewer's own verdict and rationale. The reviewer is now named
  as the decider whenever it answered.
