- **Published crates no longer resolve through yanked `arrayref` releases.**
  Harn now requires BLAKE3 1.8.7, which removed that dependency, so fresh
  installations and package verification resolve from the live registry again.
