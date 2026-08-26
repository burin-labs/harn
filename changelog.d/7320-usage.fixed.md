DeepInfra, MiniMax, and xAI streaming calls now retain their final usage
counters. Harn also accepts a usage-only terminal receipt without a trailing
`[DONE]` marker, so successful calls can report token use and cost.
