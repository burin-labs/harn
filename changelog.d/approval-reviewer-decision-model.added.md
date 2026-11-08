- An approval reviewer routed to a decision model answers through typed score questions instead of the generative
  verdict schema, with the outcome still computed from the risk and authorization levels.
  `approval_review_decision_questions` and `approval_review_decision_state` expose the question set and its state.
  The calibration corpus gains 24 paired cases, and the calibration runner now fails when any non-floor case goes
  unanswered.
