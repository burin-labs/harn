- The approval reviewer accepts an opt-in decision-model screen, `reviewer.screen`. The screen settles a review only
  when it is confident and its most likely levels compute the same outcome; an error, a timeout, a refused route,
  a malformed answer, or the uncertain band goes to the reviewer. No default changes.
  `approval_review_screen_verdict`, `approval_review_decision_questions` and `approval_review_decision_state`
  expose the decision. The catalog gains the dated `openrouter/typesafe/jev-1.14-20261014` route. The calibration
  corpus gains 24 paired cases, the runner takes `--screen`, and it now fails when any non-floor case goes unanswered.
