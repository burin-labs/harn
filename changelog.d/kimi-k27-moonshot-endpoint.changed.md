- OpenRouter Kimi K2.7 Code no longer routes to Moonshot AI's own endpoints, which drop temperature, top_p,
  and seed and reject the frequency and presence penalties they advertise. The route now forwards all
  four options.
