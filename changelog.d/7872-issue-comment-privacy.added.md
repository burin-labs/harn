- **Issue comments are scanned for public-metadata leaks (#7872).** The privacy
  gate covered pull request titles, bodies and commits but nothing scanned issue
  or pull request comments, so the same wording blocked in a body was
  publishable in a comment. Comments and issue bodies now run through the same
  vocabulary and hashed host denylist as the existing gate.
