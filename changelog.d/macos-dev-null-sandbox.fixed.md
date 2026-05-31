Allow standard process device files such as `/dev/null` through the macOS and
Linux process sandboxes without granting broad `/dev` access, and mediate Linux
device ioctls on kernels that support Landlock ABI 5.
