Code Mode validation binds to the server's prepared schemas, including shared
schema references, and refuses calls without a prepared catalog entry. A
caller-supplied schema cannot weaken or replace the published input contract.
