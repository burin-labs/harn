An agent run's opening user turn is recorded in its own LLM transcript again.
The turn was injected before the run's transcript directory became current, so
it was written nowhere, and a training example projected from that transcript
lost its `user` message.
