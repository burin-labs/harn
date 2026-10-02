- Runtime bumps no longer expose the driver's GitHub App identity to caller-owned refresh and
  validation commands as `GITHUB_APP_ID` and `GITHUB_INSTALLATION_ID`. A package that reads those
  names chose App authentication in its own tests and failed validation only inside the bump.
