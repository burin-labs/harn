The `strict` and `local-ml` security tiers now bundle the origin-provenance
defenses — directive authentication, untrusted-origin file taint, command-read
taint, and the precise (destination-aware) exfil gate — on from the mode alone.
Previously these opt-in flags had no runtime install path (`policy_from_dict`
dropped three of them and no caller set them), so the defenses were reachable
only from `#[cfg(test)]`. Command-read taint is now structurally gated on file
taint, so the inert "command reads without file provenance" combination can no
longer be configured. The default `spotlight` posture is unchanged.
