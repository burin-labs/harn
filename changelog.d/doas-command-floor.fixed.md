The command policy's catastrophic floor now looks through `doas` the way it does `sudo`,
so `doas rm -rf /` is refused and `doas` carries the privilege label.
