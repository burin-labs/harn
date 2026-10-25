# Provision an owned E2E runner's namespace grant

Use this procedure on Ubuntu hosts that restrict unprivileged user namespaces.
The slow CLI suite requires the real namespace confinement case to execute.
Keep `/proc/sys/kernel/apparmor_restrict_unprivileged_userns` enabled.

1. As the runner account, find each runner checkout's CLI path:

   ```sh
   cargo metadata --locked --no-deps --format-version 1 | jq -r '.target_directory + "/debug/harn"'
   ```

2. Have the host administrator review `scripts/ci/e2e_namespace_profile.sh`
   from the intended commit, then run its `install` command as root with all
   selected absolute CLI paths. For example:

   ```sh
   sudo bash scripts/ci/e2e_namespace_profile.sh install /opt/actions/runner-01/_work/harn/harn/target/debug/harn
   ```

   The installer replaces the one `harn-ci-e2e-userns` profile and its
   root-owned receipt. Supply the complete set of this host's attachments on
   every update. It grants `userns` only to those literal executable paths.
   It adds no sudoers permission for the runner account.

3. As the runner account, run `verify` with the selected CLI path. This checks
   protected file ownership, the exact attachment, and the installed profile's
   digest. A changed checkout or Cargo target path needs an administrator
   update.

4. Run the slow CLI workflow on the owned runner. The workflow supplies
   `HARN_EXT_OWNER_DEATH_NETNS_LAUNCHER`, making a missing namespace capability
   a failure rather than a skipped case. Confirm that
   `owner_death_preserves_namespace_then_confinement` executes and passes with
   the host restriction still enabled. The receipt alone does not prove the
   kernel loaded the grant.

The workflow never unloads this host-installed profile. Disposable hosted
jobs retain their separate temporary profile and remove it only after their
own successful installation.
