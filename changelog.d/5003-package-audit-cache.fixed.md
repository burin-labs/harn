- Warm and persist the Linux `package-audit` rust-cache on the post-merge
  refresh job (alongside `workspace-tests`) so exact-SHA proof reuse cannot leave
  the merge-gate package lane compiling cold under the 10 GiB budget (#5003).
