Dispatch sessions that stop polling and capturing are now forgotten after the retention period,
and their unaccepted captures are cancelled, so dead sessions can no longer fill the inbox's
monitor records and refuse new starts. A full inbox also frees the records of quiet sessions when
a monitor is cancelled, and a refusal forgets nothing. A host that was only silent starts again
at its next capture or claim, and can pass `--interval-seconds` to keep its pacing.
