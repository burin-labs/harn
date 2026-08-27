DeepInfra, Fireworks, Hugging Face, MiniMax, Moonshot, SambaNova, and xAI
streaming calls now retain their final usage counters. Harn also accepts a
usage-only terminal receipt without a trailing `[DONE]` marker, so successful
calls can report token use and cost. Total-only receipts retain their measured
total without fabricating input or output counts. Hugging Face calls no longer
use a fixed price because its router selects upstream providers with different
rates.
