- **A held-back tool call reads as routine ordering.** When a response mixes read and write calls, the call the agent
  loop holds for the next turn now tells the model in plain words that nothing went wrong and that it should not
  mention the hold in its reply. The old note named internal phases and read like an error, and models repeated it
  to users.
