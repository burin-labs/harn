Prepared package generations can be used from read-only application bundles
without creating installation locks. Shared readers retain publication and
generation leases; initializers that need to publish reload package authority
after acquiring the installation lock.
