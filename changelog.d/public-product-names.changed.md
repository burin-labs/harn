- **Public contract files no longer name a specific downstream host product.**
  Security policy, workflow comments, and `.gitignore` use host-neutral wording,
  and `make check-public-product-names` fails if those files grow a product
  identifier again.
