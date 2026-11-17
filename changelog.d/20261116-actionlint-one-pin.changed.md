The local workflow lint and developer setup now read the actionlint version
from the CI workflow's `ACTIONLINT_VERSION`, so bumping that one value moves
CI, `make lint-actions-source`, and `scripts/dev_setup.sh` together.
