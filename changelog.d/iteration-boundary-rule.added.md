Agent iteration events now carry `inference_boundary_rule`, the privacy rule
that admitted the call (for example `inference_boundary.any_hosted`), so a host
can record which setting let each request leave the machine. The key is absent
when no ceiling applied.
