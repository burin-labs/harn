- The never-approvable command floor now blocks `rm -rf` and `find -delete` of a
  workspace root spelled as an absolute path, or of any ancestor of a root.
  Before, `rm -rf /abs/project` read as a path inside the workspace and was
  allowed; with the home directory as the root, so was `rm -rf /home/<user>`.
