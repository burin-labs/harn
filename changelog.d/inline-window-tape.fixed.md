A recorded dispatch stays servable when a command started with `background_after_ms` finishes
inside its window. It becomes unservable only when the command returns `status: "running"`.
