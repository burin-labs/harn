- Fixed supervisor shutdown so `supervisor_stop` does not leave not-yet-started
  child tasks in a pending state under load.
