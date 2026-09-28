- OpenAI Responses-API routes stream their visible text to delta listeners as
  the model writes, instead of handing it over once at the end. The final
  response is parsed by the same code as a non-streamed reply.
