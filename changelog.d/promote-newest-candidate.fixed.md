- Fix releases stranding on one failed candidate. While main declares an
  unpublished stable version, promotion publishes the first successful main
  candidate of that version, whichever push, schedule or request built it, and
  requests a fresh source candidate whenever none is in flight. Each candidate
  still needs full-suite CI at its own source, so a red source never publishes.
